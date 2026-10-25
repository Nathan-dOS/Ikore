//! Settings pages of stage composition (pipeline v4, ADR-022): the per-project opt-in, host
//! validation with its command profiles and the optional specialty overrides, plus the Labs page
//! with the experimental knobs. Everything here edits host-owned configuration through the core;
//! nothing reaches a model.

use gpui::{AnyElement, Context, IntoElement, div, prelude::*};
use hvc_core::orchestration::composition::settings::CompositionSettings;
use hvc_core::orchestration::composition::{ExecutorKind, ValidationCommandProfileV1};
use hvc_core::terminals::{AgentExecutionMode, CliBackend, SpecialtyOverride, ThinkingLevel};

use crate::app_model::AppModel;
use crate::settings::{CompositionProjectSettings, SettingsPage};
use crate::settings_ui::{
    PageContent, hint, note, readout, row, row_with, section, select, stacked_row, switch,
};
use crate::theme;

impl AppModel {
    pub(crate) fn preview_sanitization_file(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy || self.settings.read(cx).sanitization_preview_loading {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        let path = self
            .sanitization_path_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        if path.is_empty() {
            return;
        }
        let generation = self.settings.update(cx, |settings, _| {
            settings.sanitization_preview_generation =
                settings.sanitization_preview_generation.saturating_add(1);
            settings.sanitization_preview = None;
            settings.sanitization_preview_loading = true;
            settings.error = None;
            settings.sanitization_preview_generation
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = backend
                .preview_sanitization_file(project.project_id.clone(), path.clone())
                .await;
            let _ = this.update(cx, |model, cx| {
                let current_path = model
                    .sanitization_path_input
                    .read(cx)
                    .value()
                    .trim()
                    .to_owned();
                model.settings.update(cx, |settings, _| {
                    if settings.sanitization_preview_generation != generation {
                        return;
                    }
                    settings.sanitization_preview_loading = false;
                    let current_project = settings
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.composition.as_ref())
                        .map(|s| &s.project_id);
                    if current_project != Some(&project.project_id) || current_path != path {
                        return;
                    }
                    match result {
                        Ok(preview) => {
                            settings.sanitization_preview = Some((project.project_id, preview))
                        }
                        Err(error) => settings.error = Some(error),
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn authorize_synthetic_occurrence(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        let Some((preview_project, preview)) = self.settings.read(cx).sanitization_preview.clone()
        else {
            return;
        };
        let path = self.sanitization_path_input.read(cx).value().trim();
        let reason = self
            .sanitization_reason_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        if preview_project != project.project_id
            || path != preview.relative_path
            || reason.is_empty()
        {
            return;
        }
        let Some(span) = preview.spans.get(index) else {
            return;
        };
        if project.sanitization_exceptions.iter().any(|e| {
            e.active
                && e.relative_path == preview.relative_path
                && e.original_file_sha256 == preview.original_file_sha256
                && e.start_byte == span.start_byte
                && e.end_byte == span.end_byte
                && e.detector_id == span.detector_id
                && e.detector_version == span.detector_version
        }) {
            return;
        }
        let request = hvc_core::sanitization::CreateSecretRedactionException {
            project_id: project.project_id,
            relative_path: preview.relative_path,
            detector_id: span.detector_id.clone(),
            detector_version: span.detector_version,
            original_file_sha256: preview.original_file_sha256,
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            reason,
        };
        self.run_settings_operation(Box::pin(async move {
            backend.create_sanitization_exception(request).await?;
            Ok("Synthetic occurrence authorized locally for this exact file version. New calls use the updated policy.".into())
        }), cx);
    }

    pub(crate) fn remove_sanitization_exception(
        &mut self,
        exception_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend
                    .remove_sanitization_exception(project.project_id, exception_id)
                    .await?;
                Ok(
                    "Synthetic occurrence authorization removed. New calls use the updated policy."
                        .into(),
                )
            }),
            cx,
        );
    }

    fn composition_project(&self, cx: &Context<Self>) -> Option<CompositionProjectSettings> {
        self.settings
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.composition.clone())
    }

    fn save_composition(
        &mut self,
        mutate: impl FnOnce(&mut CompositionSettings),
        notice: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        let mut settings = project.settings;
        mutate(&mut settings);
        let project_id = project.project_id;
        self.run_settings_operation(
            Box::pin(async move {
                backend.update_composition(project_id, settings).await?;
                Ok(notice.into())
            }),
            cx,
        );
    }

    pub(crate) fn toggle_composition_enabled(&mut self, cx: &mut Context<Self>) {
        self.save_composition(
            |settings| settings.enabled = !settings.enabled,
            "Stage composition updated; only batches created from now on are affected.",
            cx,
        );
    }

    pub(crate) fn start_cli_observation_trial(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend
                    .start_cli_observation_trial(project.project_id)
                    .await?;
                Ok("Unbounded CLI observation started for the next 20 Pulsation CLI runs.".into())
            }),
            cx,
        );
    }

    /// The in-flight cap of the parallel engine (ADR-022, M6): `1` keeps the
    /// sequential engine, `2` lets independent units of a stage run
    /// together. Off by default; parity with the sequential result is
    /// recorded in the validation report.
    pub(crate) fn set_composition_max_in_flight(&mut self, cap: u32, cx: &mut Context<Self>) {
        let cap = cap.clamp(1, 2);
        self.save_composition(
            move |settings| settings.max_in_flight_units = cap,
            "In-flight unit cap updated; it applies to the next composed stage admission.",
            cx,
        );
    }

    /// Promotes or demotes a planning recipe (`code` / `test`) after the
    /// developer read the qualification report (ADR-022, M7). An unpromoted
    /// recipe keeps one unit per stage.
    pub(crate) fn toggle_composition_recipe(
        &mut self,
        recipe: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.save_composition(
            move |settings| {
                if let Some(index) = settings.qualified_recipes.iter().position(|r| r == recipe) {
                    settings.qualified_recipes.remove(index);
                } else {
                    settings.qualified_recipes.push(recipe.to_owned());
                }
            },
            "Recipe qualification updated; it applies to the next composed stage admission.",
            cx,
        );
    }

    pub(crate) fn toggle_composition_visualizer(&mut self, cx: &mut Context<Self>) {
        self.save_composition(
            |settings| settings.visualizer_enabled = !settings.visualizer_enabled,
            "Visualizer notes updated; they apply to the next checkpoint that opens.",
            cx,
        );
    }

    /// The human revision rounds after which each further revision needs an
    /// explicit decision (ADR-032 M4). The host reads it at each decision.
    pub(crate) fn set_revision_round_threshold(&mut self, threshold: u32, cx: &mut Context<Self>) {
        let range = hvc_core::orchestration::composition::settings::REVISION_ROUND_THRESHOLD_RANGE;
        let threshold = threshold.clamp(*range.start(), *range.end());
        self.save_composition(
            move |settings| settings.revision_round_threshold = threshold,
            "Revision-round threshold updated; it applies to the next revision decision.",
            cx,
        );
    }

    pub(crate) fn toggle_composition_validation(&mut self, cx: &mut Context<Self>) {
        self.save_composition(
            |settings| settings.validation_enabled = !settings.validation_enabled,
            "Host validation updated; it applies to the next composed stage admission.",
            cx,
        );
    }

    /// Adds every detected preset that is not configured yet, enabled.
    pub(crate) fn add_composition_presets(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        let missing: Vec<ValidationCommandProfileV1> = project
            .presets
            .iter()
            .filter(|preset| {
                !project
                    .profiles
                    .iter()
                    .any(|(profile, _)| profile.check_ref == preset.check_ref)
            })
            .cloned()
            .collect();
        if missing.is_empty() {
            return;
        }
        let project_id = project.project_id;
        self.run_settings_operation(
            Box::pin(async move {
                let count = missing.len();
                for preset in missing {
                    backend
                        .upsert_validation_profile(project_id.clone(), preset, true)
                        .await?;
                }
                Ok(format!("{count} detected preset(s) added and enabled."))
            }),
            cx,
        );
    }

    pub(crate) fn toggle_composition_profile(&mut self, check_ref: String, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        let Some((profile, enabled)) = project
            .profiles
            .iter()
            .find(|(profile, _)| profile.check_ref.as_str() == check_ref)
            .cloned()
        else {
            return;
        };
        let project_id = project.project_id;
        self.run_settings_operation(
            Box::pin(async move {
                backend
                    .upsert_validation_profile(project_id, profile, !enabled)
                    .await?;
                Ok(if enabled {
                    "Check disabled; it will not run on the next admission.".into()
                } else {
                    "Check enabled.".into()
                })
            }),
            cx,
        );
    }

    pub(crate) fn remove_composition_profile(&mut self, check_ref: String, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        let project_id = project.project_id;
        self.run_settings_operation(
            Box::pin(async move {
                backend
                    .delete_validation_profile(project_id, check_ref)
                    .await?;
                Ok("Check removed; receipts already recorded are kept.".into())
            }),
            cx,
        );
    }

    /// Saves the override described by the form (kind, mode, backend,
    /// thinking level, optional model).
    pub(crate) fn save_specialty_override(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        let form = self.settings.read(cx);
        let Some(kind) = ExecutorKind::parse(form.composition_override_kind.as_ref()) else {
            return;
        };
        let mode = if form.composition_override_mode.as_ref() == "cli_process" {
            AgentExecutionMode::CliProcess
        } else {
            AgentExecutionMode::ApiDirect
        };
        let backend_kind = CliBackend::parse(form.composition_override_backend.as_ref());
        let thinking = ThinkingLevel::parse(form.composition_override_thinking.as_ref())
            .unwrap_or(ThinkingLevel::Default);
        let model = self
            .composition_override_model_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let setting = SpecialtyOverride {
            execution_mode: mode,
            backend: (mode == AgentExecutionMode::CliProcess)
                .then_some(backend_kind)
                .flatten(),
            model: (!model.is_empty()).then_some(model),
            thinking_level: thinking,
        };
        let project_id = project.project_id;
        self.run_settings_operation(
            Box::pin(async move {
                backend
                    .upsert_specialty_override(project_id, kind, setting)
                    .await?;
                Ok(format!(
                    "Override for `{}` saved; it applies to the next admission.",
                    kind.as_str()
                ))
            }),
            cx,
        );
    }

    pub(crate) fn remove_specialty_override(&mut self, kind: ExecutorKind, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project) = self.composition_project(cx) else {
            return;
        };
        let project_id = project.project_id;
        self.run_settings_operation(
            Box::pin(async move {
                backend.delete_specialty_override(project_id, kind).await?;
                Ok(format!(
                    "Override for `{}` removed; the stage's own configuration applies again.",
                    kind.as_str()
                ))
            }),
            cx,
        );
    }
}

/// A Labs flag that can be put back to its default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LabsFlag {
    MaxInFlight,
    RecipeCode,
    RecipeTest,
}

impl LabsFlag {
    const ALL: [Self; 3] = [Self::MaxInFlight, Self::RecipeCode, Self::RecipeTest];

    fn id(self) -> &'static str {
        match self {
            Self::MaxInFlight => "labs-reset:max-in-flight",
            Self::RecipeCode => "labs-reset:recipe-code",
            Self::RecipeTest => "labs-reset:recipe-test",
        }
    }

    fn changed(self, settings: &CompositionSettings) -> bool {
        let default = CompositionSettings::default();
        match self {
            Self::MaxInFlight => settings.max_in_flight_units != default.max_in_flight_units,
            Self::RecipeCode => has_recipe(settings, "code") != has_recipe(&default, "code"),
            Self::RecipeTest => has_recipe(settings, "test") != has_recipe(&default, "test"),
        }
    }

    fn reset(self, settings: &mut CompositionSettings) {
        let default = CompositionSettings::default();
        match self {
            Self::MaxInFlight => settings.max_in_flight_units = default.max_in_flight_units,
            Self::RecipeCode | Self::RecipeTest => {
                let recipe = if self == Self::RecipeCode {
                    "code"
                } else {
                    "test"
                };
                settings.qualified_recipes.retain(|r| r != recipe);
                if has_recipe(&default, recipe) {
                    settings.qualified_recipes.push(recipe.to_owned());
                }
            }
        }
    }
}

fn has_recipe(settings: &CompositionSettings, recipe: &str) -> bool {
    settings.qualified_recipes.iter().any(|r| r == recipe)
}

impl AppModel {
    /// Puts one Labs flag (or every one, with `None`) back to `CompositionSettings::default()`.
    pub(crate) fn reset_labs(&mut self, flag: Option<LabsFlag>, cx: &mut Context<Self>) {
        self.save_composition(
            move |settings| match flag {
                Some(flag) => flag.reset(settings),
                None => LabsFlag::ALL.iter().for_each(|flag| flag.reset(settings)),
            },
            "Labs setting reset to its default; it applies to the next composed stage admission.",
            cx,
        );
    }
}

fn no_project(page: SettingsPage) -> PageContent {
    PageContent::new(page.label())
        .description(page.description())
        .section(
            "Project",
            section(
                "Project",
                None,
                vec![note("Open a project to configure its composed stages.")],
            ),
        )
}

/// A line inside a list (a check or an override) with its buttons on the right.
fn list_line(text: String) -> gpui::Div {
    div()
        .p_2()
        .rounded_md()
        .bg(theme::surface_raised())
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(div().flex_1().min_w_0().text_sm().child(text))
}

fn sanitization_controls(
    model: &AppModel,
    project: &CompositionProjectSettings,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let settings = model.settings.read(cx);
    let loading = settings.sanitization_preview_loading;
    let preview = settings.sanitization_preview.clone();
    let path = model
        .sanitization_path_input
        .read(cx)
        .value()
        .trim()
        .to_owned();
    let reason_ready = !model
        .sanitization_reason_input
        .read(cx)
        .value()
        .trim()
        .is_empty();
    let summary = &project.sanitization_summary;
    let mut controls = div().flex().flex_col().gap_3()
        .child(note("Protection is always active. Known credential patterns are masked before model input and evidence export; unknown formats may be missed. Sensitive files remain blocked."))
        .child(readout(format!("Project history · {} inputs examined · {} input occurrences masked · {} output occurrences masked · {} fixture authorizations applied · {} bytes examined · {} ms", summary.entries_examined, summary.input_redaction_count, summary.output_redaction_count, summary.exceptions_applied, summary.examined_bytes, summary.duration_ms)))
        .when(!summary.categories.is_empty(), |v| v.child(hint(format!("Detected categories: {}", summary.categories.join(", ")))))
        .child(note("Authorize only a synthetic test fixture. Approval is local to this project, file version, detector and occurrence; changing the file makes it inactive. The matched value is never shown here."))
        .child(div().flex().flex_wrap().items_center().gap_2()
            .child(crate::sizing::sized_input(model.sanitization_path_input.clone(), crate::sizing::INPUT_LONG))
            .child(crate::settings_view::action("sanitization-preview", if loading { "Inspecting…" } else { "Inspect file" }, !busy && !loading && !path.is_empty(), cx, |model, cx| model.preview_sanitization_file(cx))))
        .child(crate::sizing::sized_input(model.sanitization_reason_input.clone(), crate::sizing::INPUT_LONG));
    if let Some((_, preview)) =
        preview.filter(|(id, p)| id == &project.project_id && p.relative_path == path)
    {
        if preview.spans.is_empty() {
            controls = controls.child(hint(
                "No catalogued secret occurrence was detected in this file.",
            ));
        }
        for (index, span) in preview.spans.iter().enumerate() {
            let allowed = project.sanitization_exceptions.iter().any(|e| {
                e.active
                    && e.relative_path == preview.relative_path
                    && e.original_file_sha256 == preview.original_file_sha256
                    && e.start_byte == span.start_byte
                    && e.end_byte == span.end_byte
                    && e.detector_id == span.detector_id
                    && e.detector_version == span.detector_version
            });
            controls = controls.child(
                list_line(format!(
                    "{} · bytes {}–{} · {} v{}",
                    span.category,
                    span.start_byte,
                    span.end_byte,
                    span.detector_id,
                    span.detector_version
                ))
                .child(crate::widgets::btn(
                    gpui::SharedString::from(format!("sanitization-authorize-{index}")),
                    if allowed {
                        "Authorized"
                    } else {
                        "Authorize synthetic fixture"
                    },
                    crate::widgets::ButtonVariant::Secondary,
                    !busy && !loading && reason_ready && !allowed,
                    cx,
                    move |model, cx| model.authorize_synthetic_occurrence(index, cx),
                )),
            );
        }
    }
    if project.sanitization_exceptions.is_empty() {
        controls = controls.child(hint("No local fixture authorization is configured."));
    }
    for exception in &project.sanitization_exceptions {
        let id = exception.id.clone();
        controls = controls.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    list_line(format!(
                        "{} · {} · bytes {}–{} · {}",
                        exception.relative_path,
                        exception.detector_id,
                        exception.start_byte,
                        exception.end_byte,
                        if exception.active {
                            "active for this file version"
                        } else {
                            "inactive: file or occurrence changed"
                        }
                    ))
                    .child(crate::widgets::btn(
                        gpui::SharedString::from(format!("sanitization-remove-{id}")),
                        "Remove",
                        crate::widgets::ButtonVariant::Danger,
                        !busy,
                        cx,
                        move |model, cx| model.remove_sanitization_exception(id.clone(), cx),
                    )),
                )
                .child(hint(format!("Reason: {}", exception.reason))),
        );
    }
    controls.into_any_element()
}

/// Settings → Stage composition: the per-project opt-in, host validation with its checks, and
/// specialty overrides. The experimental knobs live on the Labs page.
pub(crate) fn composition_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let (busy, project, override_kind, override_mode, override_backend, override_thinking) = {
        let settings = model.settings.read(cx);
        (
            settings.busy,
            settings
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.composition.clone()),
            settings.composition_override_kind.clone(),
            settings.composition_override_mode.clone(),
            settings.composition_override_backend.clone(),
            settings.composition_override_thinking.clone(),
        )
    };
    let Some(project) = project else {
        return no_project(SettingsPage::Composition);
    };
    let has_missing_presets = project.presets.iter().any(|preset| {
        !project
            .profiles
            .iter()
            .any(|(profile, _)| profile.check_ref == preset.check_ref)
    });
    let override_mode_cli = override_mode.as_ref() == "cli_process";
    let sanitization = sanitization_controls(model, &project, busy, cx);

    let checks = div()
        .flex()
        .flex_col()
        .gap_1()
        .when(project.profiles.is_empty(), |list| {
            list.child(hint("No check configured."))
        })
        .children(project.profiles.iter().map(|(profile, profile_enabled)| {
            let check_ref = profile.check_ref.as_str().to_owned();
            let remove_ref = check_ref.clone();
            list_line(format!(
                "`{}` · {:?} · {} · {} {}",
                profile.check_ref.as_str(),
                profile.kind,
                if profile.required {
                    "required"
                } else {
                    "optional"
                },
                profile.executable,
                profile.argv.join(" ")
            ))
            .child(crate::widgets::btn(
                gpui::SharedString::from(format!("composition-profile-toggle-{check_ref}")),
                if *profile_enabled { "Disable" } else { "Enable" },
                crate::widgets::ButtonVariant::Secondary,
                !busy,
                cx,
                move |model, cx| model.toggle_composition_profile(check_ref.clone(), cx),
            ))
            .child(crate::widgets::btn(
                gpui::SharedString::from(format!("composition-profile-remove-{remove_ref}")),
                "Remove",
                crate::widgets::ButtonVariant::Danger,
                !busy,
                cx,
                move |model, cx| model.remove_composition_profile(remove_ref.clone(), cx),
            ))
        }))
        .when(has_missing_presets, |list| {
            list.child(div().pt_1().child(crate::settings_view::action(
                "composition-add-presets",
                "Add detected presets (Cargo)",
                !busy,
                cx,
                |model, cx| model.add_composition_presets(cx),
            )))
        })
        .when(project.presets.is_empty(), |list| {
            list.child(hint(
                "No preset detected: the project has no Cargo.toml. Custom profiles must name a direct executable (never a .cmd/.bat wrapper).",
            ))
        });

    let overrides = div()
        .flex()
        .flex_col()
        .gap_2()
        .children(project.overrides.iter().map(|(kind, setting)| {
            let kind = *kind;
            list_line(format!(
                "`{}` · {} · {} · model {} · thinking {}",
                kind.as_str(),
                setting.execution_mode.as_str(),
                setting.backend.map(|b| b.label()).unwrap_or("api"),
                setting.model.as_deref().unwrap_or("inherited"),
                setting.thinking_level.as_str()
            ))
            .child(crate::widgets::btn(
                gpui::SharedString::from(format!("composition-override-remove-{}", kind.as_str())),
                "Remove",
                crate::widgets::ButtonVariant::Danger,
                !busy,
                cx,
                move |model, cx| model.remove_specialty_override(kind, cx),
            ))
        }))
        .child(
            div()
                .grid()
                .grid_cols(2)
                .gap_x_4()
                .gap_y_2()
                .child(crate::select_menu::field_control(
                    "composition-override-kind",
                    "SPECIALTY",
                    override_kind,
                    !busy,
                    cx,
                ))
                .child(crate::select_menu::field_control(
                    "composition-override-mode",
                    "TRANSPORT",
                    override_mode,
                    !busy,
                    cx,
                ))
                .child(crate::select_menu::field_control(
                    "composition-override-backend",
                    "CLI BACKEND",
                    override_backend,
                    !busy && override_mode_cli,
                    cx,
                ))
                .child(crate::select_menu::field_control(
                    "composition-override-thinking",
                    "THINKING",
                    override_thinking,
                    !busy,
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(crate::sizing::sized_input(
                    model.composition_override_model_input.clone(),
                    crate::sizing::INPUT_MEDIUM,
                ))
                .child(crate::settings_view::action(
                    "composition-override-save",
                    "Save override",
                    !busy,
                    cx,
                    |model, cx| model.save_specialty_override(cx),
                )),
        );

    PageContent::new(SettingsPage::Composition.label())
        .description("Changes affect batches created and stages admitted from now on.")
        .meta(format!("Project · {}", project.project_name))
        .section(
            "Secret protection",
            section(
                "Secret protection",
                None,
                vec![stacked_row("secret-sanitization", hl, sanitization)],
            ),
        )
        .section(
            "Composed batches",
            section(
                "Composed batches",
                None,
                vec![row(
                    "composition-enabled",
                    hl,
                    switch(
                        "composition-enabled",
                        project.settings.enabled,
                        !busy,
                        cx,
                        |model, cx| model.toggle_composition_enabled(cx),
                    ),
                )],
            ),
        )
        .section(
            "Host validation",
            section(
                "Host validation",
                None,
                vec![
                    row(
                        "composition-validation",
                        hl,
                        switch(
                            "composition-validation",
                            project.settings.validation_enabled,
                            !busy,
                            cx,
                            |model, cx| model.toggle_composition_validation(cx),
                        ),
                    ),
                    stacked_row("composition-checks", hl, checks),
                ],
            ),
        )
        .section(
            "Review brief",
            section(
                "Review brief",
                None,
                vec![row(
                    "composition-visualizer",
                    hl,
                    switch(
                        "composition-visualizer",
                        project.settings.visualizer_enabled,
                        !busy,
                        cx,
                        |model, cx| model.toggle_composition_visualizer(cx),
                    ),
                )],
            ),
        )
        .section(
            "Revision rounds",
            section(
                "Revision rounds",
                None,
                vec![row(
                    "composition-revision-rounds",
                    hl,
                    revision_round_stepper(project.settings.revision_round_threshold, !busy, cx),
                )],
            ),
        )
        .section(
            "Specialty overrides",
            section(
                "Specialty overrides",
                None,
                vec![stacked_row("composition-overrides", hl, overrides)],
            ),
        )
}

/// `−  n  +` for the revision-round threshold, within its range.
fn revision_round_stepper(threshold: u32, enabled: bool, cx: &mut Context<AppModel>) -> AnyElement {
    let range = hvc_core::orchestration::composition::settings::REVISION_ROUND_THRESHOLD_RANGE;
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(crate::widgets::btn(
            "composition-revision-rounds-down",
            "−",
            crate::widgets::ButtonVariant::Ghost,
            enabled && threshold > *range.start(),
            cx,
            move |model, cx| model.set_revision_round_threshold(threshold - 1, cx),
        ))
        .child(
            div()
                .debug_selector(|| "composition-revision-rounds-value".into())
                .min_w(gpui::px(24.0))
                .text_center()
                .child(threshold.to_string()),
        )
        .child(crate::widgets::btn(
            "composition-revision-rounds-up",
            "+",
            crate::widgets::ButtonVariant::Ghost,
            enabled && threshold < *range.end(),
            cx,
            move |model, cx| model.set_revision_round_threshold(threshold + 1, cx),
        ))
        .into_any_element()
}

/// One Labs row: the entry's text plus "Default: …" once changed, and a Reset beside the control.
struct LabsRow {
    entry: &'static str,
    flag: LabsFlag,
    default: &'static str,
    changed: bool,
    control: AnyElement,
}

fn labs_row(labs: LabsRow, hl: Option<&str>, busy: bool, cx: &mut Context<AppModel>) -> AnyElement {
    let entry = crate::settings_search::entry_by_id(labs.entry);
    let flag = labs.flag;
    row_with(
        entry.id,
        entry.title.into(),
        Some(entry.description.into()),
        labs.changed
            .then(|| format!("Changed · default {}", labs.default).into()),
        hl == Some(entry.id),
        div()
            .flex()
            .items_center()
            .gap_2()
            .when(labs.changed, |controls| {
                controls.child(crate::widgets::btn(
                    flag.id(),
                    "Reset",
                    crate::widgets::ButtonVariant::Ghost,
                    !busy,
                    cx,
                    move |model, cx| model.reset_labs(Some(flag), cx),
                ))
            })
            .child(labs.control),
    )
}

/// Settings → Labs: experimental switches, each saying what it changes, with a reset.
pub(crate) fn labs_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let (busy, project) = {
        let settings = model.settings.read(cx);
        (
            settings.busy,
            settings
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.composition.clone()),
        )
    };
    let Some(project) = project else {
        return no_project(SettingsPage::Labs);
    };
    let settings = project.settings.clone();
    let any_changed = LabsFlag::ALL.iter().any(|flag| flag.changed(&settings));
    let rows = [
        LabsRow {
            entry: "labs-max-in-flight",
            flag: LabsFlag::MaxInFlight,
            default: "1",
            changed: LabsFlag::MaxInFlight.changed(&settings),
            control: select(
                "composition-max-in-flight",
                settings.max_in_flight_units.to_string(),
                !busy,
                cx,
            ),
        },
        LabsRow {
            entry: "labs-recipe-code",
            flag: LabsFlag::RecipeCode,
            default: "Off",
            changed: LabsFlag::RecipeCode.changed(&settings),
            control: switch(
                "composition-recipe-code",
                has_recipe(&settings, "code"),
                !busy,
                cx,
                |model, cx| model.toggle_composition_recipe("code", cx),
            ),
        },
        LabsRow {
            entry: "labs-recipe-test",
            flag: LabsFlag::RecipeTest,
            default: "Off",
            changed: LabsFlag::RecipeTest.changed(&settings),
            control: switch(
                "composition-recipe-test",
                has_recipe(&settings, "test"),
                !busy,
                cx,
                |model, cx| model.toggle_composition_recipe("test", cx),
            ),
        },
    ];
    let experiments: Vec<AnyElement> = rows
        .into_iter()
        .map(|labs| labs_row(labs, hl, busy, cx))
        .collect();

    let trial_control = match project.cli_observation_trial.as_ref() {
        Some(trial) => readout(format!(
            "{} / {} runs",
            trial.admitted_runs, trial.target_runs
        )),
        None => crate::settings_view::action(
            "cli-observation-trial",
            "Start trial",
            !busy,
            cx,
            |model, cx| model.start_cli_observation_trial(cx),
        )
        .into_any_element(),
    };
    let mut observation = vec![row("labs-cli-observation", hl, trial_control)];
    if let Some(report) = project.cli_observation_report.clone() {
        observation.push(observation_report(report));
    }

    PageContent::new(SettingsPage::Labs.label())
        .description(SettingsPage::Labs.description())
        .action(crate::settings_view::action_with(
            "labs-reset-all",
            "Reset all",
            crate::widgets::ButtonVariant::Secondary,
            any_changed && !busy,
            cx,
            |model, cx| model.reset_labs(None, cx),
        ))
        .section(
            "Stage composition experiments",
            section(
                "Stage composition experiments",
                Some("Qualification-gated: off (or 1) until the benchmark report records them."),
                experiments,
            ),
        )
        .section(
            "CLI observation trial",
            section("CLI observation trial", None, observation),
        )
}

fn observation_report(report: hvc_core::terminals::CliObservationReport) -> AnyElement {
    let duration = |value: Option<u64>| {
        value
            .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
            .unwrap_or_else(|| "—".into())
    };
    let memory = |value: Option<u64>| {
        value
            .map(|bytes| format!("{:.1} MiB", bytes as f64 / 1_048_576.0))
            .unwrap_or_else(|| "—".into())
    };
    let rate = |value: Option<f64>| {
        value
            .map(|tps| format!("{tps:.0} tok/s"))
            .unwrap_or_else(|| "—".into())
    };
    let mut lines = vec![
        format!(
            "admitted {} · settled {} · accepted {} · rejected by stage {} · failed {} · cancelled {} · active {} · {} build",
            report.trial.admitted_runs,
            report.trial.settled_runs,
            report.successful_runs,
            report.stage_rejected_runs,
            report.failed_runs,
            report.cancelled_runs,
            report.active_runs,
            report.build_profile,
        ),
        format!(
            "accepted duration: mean {} · median {} · P95 {} · max {}; longest silence: transport {} · frames {}",
            duration(report.mean_success_duration_ms),
            duration(report.median_success_duration_ms),
            duration(report.p95_success_duration_ms),
            duration(report.max_success_duration_ms),
            duration(report.max_transport_silence_ms),
            duration(report.max_frame_silence_ms),
        ),
        format!(
            "Job Object peak memory: process {} · job {}. P95 is withheld until three accepted samples.",
            memory(report.peak_process_memory_bytes),
            memory(report.peak_job_memory_bytes),
        ),
    ];
    for group in &report.groups {
        let spread = if group.accepted_durations_ms.is_empty() {
            format!(
                "mean {} · P95 {} · max {}",
                duration(group.mean_accepted_duration_ms),
                duration(group.p95_accepted_duration_ms),
                duration(group.max_accepted_duration_ms),
            )
        } else {
            group
                .accepted_durations_ms
                .iter()
                .map(|ms| duration(Some(*ms)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(format!(
            "{} · {}: {} accepted ({spread}) · {} other · {} · transport silence {}",
            group.agent_role,
            group.backend,
            group.accepted_runs,
            group.other_runs,
            rate(group.mean_tokens_per_second),
            duration(group.max_transport_silence_ms),
        ));
    }
    for run in report.runs.iter().filter(|run| run.model_mismatch) {
        lines.push(format!(
            "{} requested `{}` but `{}` did the work.",
            run.agent_role,
            run.requested_model.as_deref().unwrap_or("—"),
            run.observed_model.as_deref().unwrap_or("—"),
        ));
    }
    for pulsation in &report.pulsations {
        lines.push(format!(
            "{}: total {} = CLI {} · preparation {} · admission {} · human {} · other {}",
            pulsation.feature_name,
            duration(Some(pulsation.total_ms)),
            duration(Some(pulsation.cli_ms)),
            duration(Some(pulsation.prep_ms)),
            duration(Some(pulsation.admission_ms)),
            duration(Some(pulsation.human_wait_ms)),
            duration(Some(pulsation.other_ms)),
        ));
    }
    div()
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_subtle())
                .child("OBSERVATION REPORT (READ-ONLY)"),
        )
        .children(lines.into_iter().map(readout))
        .into_any_element()
}
