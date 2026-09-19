//! Settings surface of stage composition (pipeline v4, ADR-022): the
//! per-project opt-in, host validation with its command profiles, and the
//! optional specialty overrides. Everything here edits host-owned
//! configuration through the core; nothing reaches a model.

use gpui::{AnyElement, Context, IntoElement, div, prelude::*};
use hvc_core::orchestration::composition::settings::CompositionSettings;
use hvc_core::orchestration::composition::{ExecutorKind, ValidationCommandProfileV1};
use hvc_core::terminals::{AgentExecutionMode, CliBackend, SpecialtyOverride, ThinkingLevel};

use crate::app_model::AppModel;
use crate::settings::CompositionProjectSettings;
use crate::theme;

impl AppModel {
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

fn line(text: impl Into<String>) -> AnyElement {
    div()
        .text_xs()
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

/// The "Stage composition (v4)" card of the Runtime section.
pub(crate) fn composition_settings_card(
    model: &AppModel,
    cx: &mut Context<AppModel>,
) -> AnyElement {
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
        return crate::settings_view::card("Stage composition (v4)")
            .child(line("Open a project to configure composed stages."))
            .into_any_element();
    };
    let enabled = project.settings.enabled;
    let validation = project.settings.validation_enabled;
    let has_missing_presets = project.presets.iter().any(|preset| {
        !project
            .profiles
            .iter()
            .any(|(profile, _)| profile.check_ref == preset.check_ref)
    });
    let override_mode_cli = override_mode.as_ref() == "cli_process";
    let model_input = model.composition_override_model_input.clone();

    crate::settings_view::card(format!("Stage composition (v4) · {}", project.project_name))
        .child(line("Batches created from Forge tasks run as host-coordinated units with a shared budget, exact edits and host checks. Free-form pulsations are not affected. Changes apply to batches created and stages admitted from now on."))
        .child(crate::settings_view::toggle(
            "composition-enabled",
            "COMPOSED BATCHES",
            if enabled { "on" } else { "off" },
            !busy,
            cx,
            |model, cx| model.toggle_composition_enabled(cx),
        ))
        .child(crate::select_menu::control(
            "composition-max-in-flight",
            "UNITS IN FLIGHT",
            project.settings.max_in_flight_units.to_string(),
            !busy,
            cx,
        ))
        .child(line("1 runs the units of a stage one at a time. 2 lets independent units (no dependency, disjoint files) run together; host checks never overlap. The result is the same either way - only wall time changes."))
        .child(div().mt_2().text_xs().text_color(theme::text_muted()).child("PLANNED UNITS (qualified recipes)"))
        .child(crate::settings_view::toggle(
            "composition-recipe-code",
            "CODE STAGE PLANS SEVERAL UNITS",
            if project.settings.qualified_recipes.iter().any(|r| r == "code") { "on" } else { "off" },
            !busy,
            cx,
            |model, cx| model.toggle_composition_recipe("code", cx),
        ))
        .child(crate::settings_view::toggle(
            "composition-recipe-test",
            "TEST STAGE PLANS SEVERAL UNITS",
            if project.settings.qualified_recipes.iter().any(|r| r == "test") { "on" } else { "off" },
            !busy,
            cx,
            |model, cx| model.toggle_composition_recipe("test", cx),
        ))
        .child(line("Off keeps one worker per stage (benchmark arm B). Turn a recipe on only after the qualification report in quality/stage-composition-benchmark-<date>.md records it; the fixture-mode report proves orchestration, not cost."))
        .child(crate::settings_view::toggle(
            "composition-validation",
            "HOST VALIDATION (compile / tests)",
            if validation { "on" } else { "off" },
            !busy,
            cx,
            |model, cx| model.toggle_composition_validation(cx),
        ))
        .child(line("Runs the checks below on a disposable copy of the project under the app data directory. Trusted-project runner: no sandbox, no downloads, no credentials forwarded. Disabled: reports show `not run` and the human review proceeds."))
        .child(div().mt_2().text_xs().text_color(theme::text_muted()).child("CHECKS"))
        .when(project.profiles.is_empty(), |card| {
            card.child(line("No check configured."))
        })
        .children(project.profiles.iter().map(|(profile, profile_enabled)| {
            let check_ref = profile.check_ref.as_str().to_owned();
            let remove_ref = check_ref.clone();
            div()
                .mt_1()
                .p_2()
                .rounded_md()
                .bg(theme::surface_raised())
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(div().text_sm().child(format!(
                    "`{}` · {:?} · {} · {} {}",
                    profile.check_ref.as_str(),
                    profile.kind,
                    if profile.required { "required" } else { "optional" },
                    profile.executable,
                    profile.argv.join(" ")
                )))
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
        .when(has_missing_presets, |card| {
            card.child(crate::settings_view::action(
                "composition-add-presets",
                "Add detected presets (Cargo)",
                !busy,
                cx,
                |model, cx| model.add_composition_presets(cx),
            ))
        })
        .when(project.presets.is_empty(), |card| {
            card.child(line("No preset detected: the project has no Cargo.toml. Custom profiles must name a direct executable (never a .cmd/.bat wrapper)."))
        })
        .child(div().mt_2().text_xs().text_color(theme::text_muted()).child("SPECIALTY OVERRIDES (optional)"))
        .child(line("A worker specialty inherits its stage's model and transport unless overridden here. The host never escalates to a costlier model or reasoning on its own."))
        .children(project.overrides.iter().map(|(kind, setting)| {
            let kind = *kind;
            div()
                .mt_1()
                .p_2()
                .rounded_md()
                .bg(theme::surface_raised())
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(div().text_sm().child(format!(
                    "`{}` · {} · {} · model {} · thinking {}",
                    kind.as_str(),
                    setting.execution_mode.as_str(),
                    setting.backend.map(|b| b.label()).unwrap_or("api"),
                    setting.model.as_deref().unwrap_or("inherited"),
                    setting.thinking_level.as_str()
                )))
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
                .mt_2()
                .grid()
                .grid_cols(4)
                .gap_2()
                .child(crate::select_menu::control(
                    "composition-override-kind",
                    "SPECIALTY",
                    override_kind,
                    !busy,
                    cx,
                ))
                .child(crate::select_menu::control(
                    "composition-override-mode",
                    "TRANSPORT",
                    override_mode,
                    !busy,
                    cx,
                ))
                .child(crate::select_menu::control(
                    "composition-override-backend",
                    "CLI BACKEND",
                    override_backend,
                    !busy && override_mode_cli,
                    cx,
                ))
                .child(crate::select_menu::control(
                    "composition-override-thinking",
                    "THINKING",
                    override_thinking,
                    !busy,
                    cx,
                )),
        )
        .child(crate::sizing::sized_input(model_input, crate::sizing::INPUT_MEDIUM).mt_2())
        .child(crate::settings_view::action(
            "composition-override-save",
            "Save specialty override",
            !busy,
            cx,
            |model, cx| model.save_specialty_override(cx),
        ))
        .into_any_element()
}
