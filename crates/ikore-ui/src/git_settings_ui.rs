//! Settings › Git (Feature 08 M1): read-only git/gh host status, editable
//! per-project commit/push/pull rules, and fixed host guardrails.
//!
//! All user-facing text is plain English with zero literal backticks.

use std::collections::BTreeMap;

use gpui::{AnyElement, Context, Entity, SharedString, div, prelude::*};
use ikore_contracts::git_tool::{
    BranchMode, COMMIT_RULES_SCHEMA_VERSION, CommitBranchRules, CommitConsistencyRules,
    CommitGranularity, CommitLimitsRules, CommitMessageRules, CommitPathsRules, CommitPullRules,
    CommitPushRules, CommitRulesV1, DocsPlacement, GitStatusV1, MessageFormat, MessageScope,
    ProjectGitRulesRecord, PullMode, REQUIRED_MESSAGE_LANGUAGE, REQUIRED_TASK_KINDS, RuleAction,
    RuleFieldError, SaveProjectGitRulesOutcome, SaveProjectGitRulesRequest, fixed_git_rules,
};

use crate::app_model::AppModel;
use crate::settings::SettingsPage;
use crate::settings_ui::{
    PageContent, hint, note, readout, row, section, stacked_row, switch, warning_note,
};
use crate::sizing::{INPUT_LONG, INPUT_MEDIUM, INPUT_SHORT, sized_input};
use crate::text_input::NativeTextInput;
use crate::theme;
use crate::widgets::{ButtonVariant, btn};

#[derive(Clone, Default)]
pub(crate) struct GitSettingsState {
    pub(crate) project_id: Option<String>,
    pub(crate) generation: u64,
    pub(crate) loading_rules: bool,
    pub(crate) loading_status: bool,
    pub(crate) saving: bool,
    pub(crate) record: Option<ProjectGitRulesRecord>,
    pub(crate) status: Option<GitStatusV1>,
    pub(crate) status_error: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) notice: Option<String>,
    pub(crate) field_errors: Vec<RuleFieldError>,
    pub(crate) form: Option<GitRulesForm>,
}

impl std::fmt::Debug for GitSettingsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitSettingsState")
            .field("project_id", &self.project_id)
            .field("generation", &self.generation)
            .field("loading_rules", &self.loading_rules)
            .field("loading_status", &self.loading_status)
            .field("saving", &self.saving)
            .field("has_record", &self.record.is_some())
            .field("has_status", &self.status.is_some())
            .field("has_form", &self.form.is_some())
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct GitRulesForm {
    pub(crate) enabled: bool,
    pub(crate) format: MessageFormat,
    pub(crate) scope: MessageScope,
    pub(crate) types: Entity<NativeTextInput>,
    pub(crate) type_by_task_kind: Vec<(String, Entity<NativeTextInput>)>,
    pub(crate) subject_max_chars: Entity<NativeTextInput>,
    pub(crate) body_required: bool,
    pub(crate) body_wrap: Entity<NativeTextInput>,
    pub(crate) pattern: Entity<NativeTextInput>,
    pub(crate) trailers: bool,
    pub(crate) sign_off: bool,
    pub(crate) granularity: CommitGranularity,
    pub(crate) max_changed_lines: Entity<NativeTextInput>,
    pub(crate) max_files: Entity<NativeTextInput>,
    pub(crate) on_exceed: RuleAction,
    pub(crate) exclude_from_count: Entity<NativeTextInput>,
    pub(crate) docs_placement: DocsPlacement,
    pub(crate) missing_tests: RuleAction,
    pub(crate) forbidden_paths: Entity<NativeTextInput>,
    pub(crate) branch_mode: BranchMode,
    pub(crate) branch_pattern: Entity<NativeTextInput>,
    pub(crate) protected_branches: Entity<NativeTextInput>,
    pub(crate) push_remote: Entity<NativeTextInput>,
    pub(crate) set_upstream: bool,
    pub(crate) pull_remote: Entity<NativeTextInput>,
}

fn new_input(
    placeholder: &str,
    value: String,
    cx: &mut Context<AppModel>,
) -> Entity<NativeTextInput> {
    cx.new(|cx| {
        let mut input = NativeTextInput::new(placeholder.to_owned(), cx);
        input.set_value(value, cx);
        input
    })
}

fn set_input(entity: &Entity<NativeTextInput>, value: String, cx: &mut Context<AppModel>) {
    entity.update(cx, |input, cx| input.set_value(value, cx));
}

fn join_csv(items: &[String]) -> String {
    items.join(", ")
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split([',', ';'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_u32_field(raw: &str, field: &str, label: &str, errors: &mut Vec<RuleFieldError>) -> u32 {
    match raw.trim().parse::<u32>() {
        Ok(value) => value,
        Err(_) => {
            errors.push(RuleFieldError::new(
                field,
                "invalid_integer",
                format!("{label} must be a whole number."),
            ));
            0
        }
    }
}

impl GitRulesForm {
    fn new(enabled: bool, rules: &CommitRulesV1, cx: &mut Context<AppModel>) -> Self {
        let type_by_task_kind = REQUIRED_TASK_KINDS
            .iter()
            .map(|kind| {
                let value = rules
                    .message
                    .type_by_task_kind
                    .get(*kind)
                    .cloned()
                    .unwrap_or_default();
                ((*kind).to_owned(), new_input("feat", value, cx))
            })
            .collect();
        Self {
            enabled,
            format: rules.message.format,
            scope: rules.message.scope,
            types: new_input("feat, fix, refactor", join_csv(&rules.message.types), cx),
            type_by_task_kind,
            subject_max_chars: new_input("72", rules.message.subject_max_chars.to_string(), cx),
            body_required: rules.message.body_required,
            body_wrap: new_input("72", rules.message.body_wrap.to_string(), cx),
            pattern: new_input("Optional regex pattern", rules.message.pattern.clone(), cx),
            trailers: rules.message.trailers,
            sign_off: rules.message.sign_off,
            granularity: rules.granularity,
            max_changed_lines: new_input("400", rules.limits.max_changed_lines.to_string(), cx),
            max_files: new_input("30", rules.limits.max_files.to_string(), cx),
            on_exceed: rules.limits.on_exceed,
            exclude_from_count: new_input(
                "Cargo.lock, package-lock.json",
                join_csv(&rules.limits.exclude_from_count),
                cx,
            ),
            docs_placement: rules.docs_placement,
            missing_tests: rules.consistency.missing_tests,
            forbidden_paths: new_input(
                ".env, .env.*, **/*.pem",
                join_csv(&rules.paths.forbidden),
                cx,
            ),
            branch_mode: rules.branch.mode,
            branch_pattern: new_input(
                "ikore/{pulsationShort}-{slug}",
                rules.branch.pattern.clone(),
                cx,
            ),
            protected_branches: new_input(
                "main, master, develop, release/*",
                join_csv(&rules.branch.protected),
                cx,
            ),
            push_remote: new_input("origin", rules.push.remote.clone(), cx),
            set_upstream: rules.push.set_upstream,
            pull_remote: new_input("origin", rules.pull.remote.clone(), cx),
        }
    }

    fn apply_record(&mut self, record: &ProjectGitRulesRecord, cx: &mut Context<AppModel>) {
        let rules = &record.rules;
        self.enabled = record.enabled;
        self.format = rules.message.format;
        self.scope = rules.message.scope;
        set_input(&self.types, join_csv(&rules.message.types), cx);
        for (kind, entity) in &self.type_by_task_kind {
            let value = rules
                .message
                .type_by_task_kind
                .get(kind)
                .cloned()
                .unwrap_or_default();
            set_input(entity, value, cx);
        }
        set_input(
            &self.subject_max_chars,
            rules.message.subject_max_chars.to_string(),
            cx,
        );
        self.body_required = rules.message.body_required;
        set_input(&self.body_wrap, rules.message.body_wrap.to_string(), cx);
        set_input(&self.pattern, rules.message.pattern.clone(), cx);
        self.trailers = rules.message.trailers;
        self.sign_off = rules.message.sign_off;
        self.granularity = rules.granularity;
        set_input(
            &self.max_changed_lines,
            rules.limits.max_changed_lines.to_string(),
            cx,
        );
        set_input(&self.max_files, rules.limits.max_files.to_string(), cx);
        self.on_exceed = rules.limits.on_exceed;
        set_input(
            &self.exclude_from_count,
            join_csv(&rules.limits.exclude_from_count),
            cx,
        );
        self.docs_placement = rules.docs_placement;
        self.missing_tests = rules.consistency.missing_tests;
        set_input(&self.forbidden_paths, join_csv(&rules.paths.forbidden), cx);
        self.branch_mode = rules.branch.mode;
        set_input(&self.branch_pattern, rules.branch.pattern.clone(), cx);
        set_input(
            &self.protected_branches,
            join_csv(&rules.branch.protected),
            cx,
        );
        set_input(&self.push_remote, rules.push.remote.clone(), cx);
        self.set_upstream = rules.push.set_upstream;
        set_input(&self.pull_remote, rules.pull.remote.clone(), cx);
    }

    fn build_commit_rules(&self, cx: &gpui::App) -> (CommitRulesV1, Vec<RuleFieldError>) {
        let mut errors = Vec::new();
        let mut type_by_task_kind = BTreeMap::new();
        for (kind, entity) in &self.type_by_task_kind {
            let mapped = entity.read(cx).value().trim().to_owned();
            if !mapped.is_empty() {
                type_by_task_kind.insert(kind.clone(), mapped);
            }
        }
        let subject_max_chars = parse_u32_field(
            self.subject_max_chars.read(cx).value(),
            "message.subjectMaxChars",
            "Subject max length",
            &mut errors,
        );
        let body_wrap = parse_u32_field(
            self.body_wrap.read(cx).value(),
            "message.bodyWrap",
            "Body wrap width",
            &mut errors,
        );
        let max_changed_lines = parse_u32_field(
            self.max_changed_lines.read(cx).value(),
            "limits.maxChangedLines",
            "Maximum changed lines",
            &mut errors,
        );
        let max_files = parse_u32_field(
            self.max_files.read(cx).value(),
            "limits.maxFiles",
            "Maximum files",
            &mut errors,
        );
        let rules = CommitRulesV1 {
            schema_version: COMMIT_RULES_SCHEMA_VERSION.to_owned(),
            message: CommitMessageRules {
                format: self.format,
                types: split_list(self.types.read(cx).value()),
                type_by_task_kind,
                scope: self.scope,
                subject_max_chars,
                body_required: self.body_required,
                body_wrap,
                pattern: self.pattern.read(cx).value().to_owned(),
                language: REQUIRED_MESSAGE_LANGUAGE.to_owned(),
                trailers: self.trailers,
                sign_off: self.sign_off,
            },
            granularity: self.granularity,
            limits: CommitLimitsRules {
                max_changed_lines,
                max_files,
                on_exceed: self.on_exceed,
                exclude_from_count: split_list(self.exclude_from_count.read(cx).value()),
            },
            docs_placement: self.docs_placement,
            consistency: CommitConsistencyRules {
                missing_tests: self.missing_tests,
            },
            paths: CommitPathsRules {
                forbidden: split_list(self.forbidden_paths.read(cx).value()),
            },
            branch: CommitBranchRules {
                mode: self.branch_mode,
                pattern: self.branch_pattern.read(cx).value().trim().to_owned(),
                protected: split_list(self.protected_branches.read(cx).value()),
            },
            push: CommitPushRules {
                remote: self.push_remote.read(cx).value().trim().to_owned(),
                set_upstream: self.set_upstream,
            },
            pull: CommitPullRules {
                remote: self.pull_remote.read(cx).value().trim().to_owned(),
                mode: PullMode::FfOnly,
            },
        };
        (rules, errors)
    }
}

impl AppModel {
    fn active_git_project_id(&self, cx: &gpui::App) -> Option<String> {
        self.workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
    }

    pub(crate) fn refresh_git_settings(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.active_git_project_id(cx) else {
            self.settings.update(cx, |settings, _| {
                settings.git = Default::default();
            });
            return;
        };

        let reset_project =
            self.settings.read(cx).git.project_id.as_deref() != Some(project_id.as_str());
        if reset_project || self.settings.read(cx).git.form.is_none() {
            let defaults = CommitRulesV1::default();
            let form = GitRulesForm::new(false, &defaults, cx);
            self.settings.update(cx, |settings, _| {
                if reset_project {
                    settings.git.record = None;
                    settings.git.status = None;
                    settings.git.status_error = None;
                    settings.git.error = None;
                    settings.git.notice = None;
                    settings.git.field_errors.clear();
                }
                settings.git.project_id = Some(project_id.clone());
                settings.git.form = Some(form);
            });
        }

        let Some(backend) = self.settings_backend.clone() else {
            return;
        };

        let generation = self.settings.update(cx, |settings, _| {
            settings.git.generation += 1;
            settings.git.loading_rules = true;
            settings.git.loading_status = true;
            settings.git.error = None;
            settings.git.status_error = None;
            settings.git.generation
        });

        let rules_backend = backend.clone();
        let rules_project = project_id.clone();
        cx.spawn(async move |this, cx| {
            let result = rules_backend.git_rules(rules_project.clone()).await;
            let _ = this.update(cx, |model, cx| {
                if model.settings.read(cx).git.generation != generation
                    || model.settings.read(cx).git.project_id.as_deref()
                        != Some(rules_project.as_str())
                {
                    return;
                }
                model.settings.update(cx, |settings, _| {
                    settings.git.loading_rules = false;
                });
                match result {
                    Ok(record) => {
                        let existing_form = model.settings.update(cx, |settings, _| {
                            settings.git.record = Some(record.clone());
                            settings.git.form.take()
                        });
                        if let Some(mut form) = existing_form {
                            form.apply_record(&record, cx);
                            model.settings.update(cx, |settings, _| {
                                settings.git.form = Some(form);
                            });
                        } else {
                            let form = GitRulesForm::new(record.enabled, &record.rules, cx);
                            model.settings.update(cx, |settings, _| {
                                settings.git.form = Some(form);
                            });
                        }
                    }
                    Err(error) => {
                        model.settings.update(cx, |settings, _| {
                            settings.git.error = Some(error.message);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();

        self.spawn_git_status_probe(backend, project_id, generation, cx);
    }

    pub(crate) fn refresh_git_status_only(&mut self, cx: &mut Context<Self>) {
        let (Some(backend), Some(project_id)) = (
            self.settings_backend.clone(),
            self.active_git_project_id(cx),
        ) else {
            return;
        };
        let generation = self.settings.update(cx, |settings, _| {
            settings.git.loading_status = true;
            settings.git.status_error = None;
            settings.git.generation
        });
        cx.notify();
        self.spawn_git_status_probe(backend, project_id, generation, cx);
    }

    fn spawn_git_status_probe(
        &self,
        backend: std::sync::Arc<dyn crate::settings::SettingsBackend>,
        project_id: String,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = backend.git_status(project_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                model.settings.update(cx, |settings, _| {
                    if settings.git.generation != generation
                        || settings.git.project_id.as_deref() != Some(project_id.as_str())
                    {
                        return;
                    }
                    settings.git.loading_status = false;
                    match result {
                        Ok(status) => {
                            settings.git.status = Some(status);
                            settings.git.status_error = None;
                        }
                        Err(error) => {
                            settings.git.status_error = Some(error.message);
                        }
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn set_git_tool_enabled(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            if let Some(form) = settings.git.form.as_mut() {
                form.enabled = !form.enabled;
            }
        });
        self.save_git_rules(cx);
    }

    pub(crate) fn save_git_rules(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.active_git_project_id(cx) else {
            return;
        };
        let (enabled, rules, local_errors) = {
            let settings = self.settings.read(cx);
            let Some(form) = settings.git.form.as_ref() else {
                return;
            };
            let (rules, local_errors) = form.build_commit_rules(cx);
            (form.enabled, rules, local_errors)
        };
        if !local_errors.is_empty() {
            self.settings.update(cx, |settings, _| {
                settings.git.field_errors = local_errors;
                settings.git.error = Some("Fix the highlighted fields before saving.".to_owned());
                settings.git.notice = None;
            });
            cx.notify();
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            self.settings.update(cx, |settings, _| {
                settings.git.field_errors.clear();
                settings.git.error = None;
                settings.git.notice = Some("Rules validated locally.".to_owned());
            });
            cx.notify();
            return;
        };

        let generation = self.settings.update(cx, |settings, _| {
            settings.git.saving = true;
            settings.git.error = None;
            settings.git.notice = None;
            settings.git.field_errors.clear();
            settings.git.generation
        });
        cx.notify();

        let request = SaveProjectGitRulesRequest {
            project_id: project_id.clone(),
            enabled,
            rules,
        };
        cx.spawn(async move |this, cx| {
            let result = backend.save_git_rules(request).await;
            let _ = this.update(cx, |model, cx| {
                if model.settings.read(cx).git.generation != generation
                    || model.settings.read(cx).git.project_id.as_deref()
                        != Some(project_id.as_str())
                {
                    return;
                }
                model.settings.update(cx, |settings, _| {
                    settings.git.saving = false;
                });
                match result {
                    Ok(SaveProjectGitRulesOutcome::Saved(record)) => {
                        let record = *record;
                        let notice = format!(
                            "Saved git rules (version {}, {}).",
                            record.version,
                            if record.enabled {
                                "enabled"
                            } else {
                                "disabled"
                            }
                        );
                        let existing_form = model.settings.update(cx, |settings, _| {
                            settings.git.record = Some(record.clone());
                            settings.git.field_errors.clear();
                            settings.git.error = None;
                            settings.git.notice = Some(notice);
                            settings.git.form.take()
                        });
                        if let Some(mut form) = existing_form {
                            form.apply_record(&record, cx);
                            model.settings.update(cx, |settings, _| {
                                settings.git.form = Some(form);
                            });
                        }
                    }
                    Ok(SaveProjectGitRulesOutcome::Invalid(errors)) => {
                        model.settings.update(cx, |settings, _| {
                            settings.git.field_errors = errors;
                            settings.git.error =
                                Some("Validation rejected one or more rule fields.".to_owned());
                            settings.git.notice = None;
                        });
                    }
                    Err(error) => {
                        model.settings.update(cx, |settings, _| {
                            settings.git.error = Some(error.message);
                            settings.git.notice = None;
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn restore_default_git_rules(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.active_git_project_id(cx) else {
            return;
        };
        let Some(backend) = self.settings_backend.clone() else {
            let defaults = CommitRulesV1::default();
            let existing_form = self
                .settings
                .update(cx, |settings, _| settings.git.form.take());
            if let Some(mut form) = existing_form {
                let record = ProjectGitRulesRecord {
                    project_id,
                    version: 0,
                    enabled: form.enabled,
                    rules: defaults,
                    rules_sha256: String::new(),
                    created_at: String::new(),
                };
                form.apply_record(&record, cx);
                self.settings.update(cx, |settings, _| {
                    settings.git.form = Some(form);
                    settings.git.field_errors.clear();
                    settings.git.error = None;
                    settings.git.notice = Some("Restored default commit rules.".to_owned());
                });
            }
            cx.notify();
            return;
        };

        let generation = self.settings.update(cx, |settings, _| {
            settings.git.saving = true;
            settings.git.error = None;
            settings.git.notice = None;
            settings.git.field_errors.clear();
            settings.git.generation
        });
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = backend.restore_default_git_rules(project_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                if model.settings.read(cx).git.generation != generation
                    || model.settings.read(cx).git.project_id.as_deref()
                        != Some(project_id.as_str())
                {
                    return;
                }
                model.settings.update(cx, |settings, _| {
                    settings.git.saving = false;
                });
                match result {
                    Ok(record) => {
                        let notice = format!(
                            "Restored default git rules (saved as version {}).",
                            record.version
                        );
                        let existing_form = model.settings.update(cx, |settings, _| {
                            settings.git.record = Some(record.clone());
                            settings.git.field_errors.clear();
                            settings.git.error = None;
                            settings.git.notice = Some(notice);
                            settings.git.form.take()
                        });
                        if let Some(mut form) = existing_form {
                            form.apply_record(&record, cx);
                            model.settings.update(cx, |settings, _| {
                                settings.git.form = Some(form);
                            });
                        }
                    }
                    Err(error) => {
                        model.settings.update(cx, |settings, _| {
                            settings.git.error = Some(error.message);
                            settings.git.notice = None;
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn update_git_form(&mut self, cx: &mut Context<Self>, apply: impl FnOnce(&mut GitRulesForm)) {
        self.settings.update(cx, |settings, _| {
            if let Some(form) = settings.git.form.as_mut() {
                apply(form);
            }
            settings.git.notice = None;
        });
        cx.notify();
    }
}

fn no_project() -> PageContent {
    PageContent::new(SettingsPage::Git.label())
        .description(SettingsPage::Git.description())
        .section(
            "Project",
            section(
                "Project",
                None,
                vec![note(
                    "Open a project to inspect git status and configure commit rules.",
                )],
            ),
        )
}

fn choice_button(
    id: &'static str,
    label: &'static str,
    selected: bool,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    btn(
        id,
        label,
        if selected {
            ButtonVariant::Selected
        } else {
            ButtonVariant::Secondary
        },
        enabled,
        cx,
        handler,
    )
}

fn field_label(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

fn field_error<'a>(errors: &'a [RuleFieldError], field: &str) -> Option<&'a RuleFieldError> {
    errors.iter().find(|err| err.field == field)
}

fn field_error_element(errors: &[RuleFieldError], field: &str) -> Option<AnyElement> {
    field_error(errors, field).map(|err| {
        div()
            .debug_selector({
                let f = err.field.clone();
                move || format!("git-field-error:{f}")
            })
            .text_xs()
            .text_color(theme::danger())
            .child(err.message.clone())
            .into_any_element()
    })
}

fn field_block(
    label: &'static str,
    control: AnyElement,
    error_key: &str,
    errors: &[RuleFieldError],
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(field_label(label))
        .child(control)
        .when_some(field_error_element(errors, error_key), |col, err| {
            col.child(err)
        })
        .into_any_element()
}

fn status_line_row(label: &'static str, value: impl Into<SharedString>, warn: bool) -> AnyElement {
    let text: SharedString = value.into();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_2()
        .child(field_label(label))
        .child(
            div()
                .min_w_0()
                .font_family(theme::FONT_MONO)
                .text_xs()
                .text_color(if warn {
                    theme::warning()
                } else {
                    theme::text()
                })
                .child(text),
        )
        .into_any_element()
}

fn render_status_body(
    state: &GitSettingsState,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let mut body = div().flex().flex_col().gap_3().child(
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(hint(if state.loading_status {
                "Inspecting git repository and installed binaries..."
            } else {
                "Read-only probe of local git, GitHub CLI (gh), repository identity, and remote."
            }))
            .child(btn(
                "git-refresh-status",
                if state.loading_status {
                    "Refreshing..."
                } else {
                    "Refresh status"
                },
                ButtonVariant::Secondary,
                !busy && !state.loading_status,
                cx,
                |model, cx| model.refresh_git_status_only(cx),
            )),
    );

    if let Some(err) = state.status_error.as_ref() {
        body = body.child(warning_note(err.clone()));
    }

    let Some(status) = state.status.as_ref() else {
        if !state.loading_status && state.status_error.is_none() {
            body = body.child(hint("Status has not been probed yet."));
        }
        return body.into_any_element();
    };

    let git_summary = if status.git.available {
        format!(
            "{} ({})",
            status.git.version.as_deref().unwrap_or("available"),
            status.git.executable_path.as_deref().unwrap_or("git")
        )
    } else {
        status
            .git
            .error
            .clone()
            .unwrap_or_else(|| "Not available".to_owned())
    };
    let gh_summary = if status.gh.available {
        format!(
            "{} ({})",
            status.gh.version.as_deref().unwrap_or("available"),
            status.gh_status_summary
        )
    } else {
        status.gh_status_summary.clone()
    };
    let identity_summary = match (&status.user_name, &status.user_email) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (Some(name), None) => format!("{name} (missing email)"),
        (None, Some(email)) => format!("(missing name) <{email}>"),
        (None, None) => "Missing git user.name and user.email".to_owned(),
    };
    let branch_summary = if !status.in_git_repository {
        "Not a git repository".to_owned()
    } else if status.detached_head {
        "Detached HEAD".to_owned()
    } else {
        status
            .current_branch
            .clone()
            .unwrap_or_else(|| "Unknown branch".to_owned())
    };

    let grid = div()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::canvas())
        .flex()
        .flex_col()
        .gap_2()
        .child(status_line_row(
            "Workspace",
            status
                .repository_root
                .clone()
                .unwrap_or_else(|| status.workspace_path.clone()),
            !status.in_git_repository,
        ))
        .child(status_line_row(
            "git binary",
            git_summary,
            !status.git.available,
        ))
        .child(status_line_row(
            "GitHub CLI (gh)",
            gh_summary,
            !status.gh.available,
        ))
        .child(status_line_row(
            "Author identity",
            identity_summary,
            status.user_name.is_none() || status.user_email.is_none(),
        ))
        .child(status_line_row(
            "Current branch",
            branch_summary,
            !status.in_git_repository || status.detached_head,
        ))
        .child(status_line_row(
            "Remote",
            status
                .remote_url
                .as_ref()
                .map(|url| format!("{} -> {url}", status.remote_name))
                .unwrap_or_else(|| format!("{} (not configured)", status.remote_name)),
            status.remote_url.is_none(),
        ))
        .child(status_line_row(
            "Credential helper",
            status
                .credential_helper
                .clone()
                .unwrap_or_else(|| "None detected".to_owned()),
            false,
        ));

    body = body.child(grid);

    if !status.setup_hints.is_empty() {
        body = body.child(
            div().flex().flex_col().gap_1().children(
                status
                    .setup_hints
                    .iter()
                    .map(|line| warning_note(line.clone())),
            ),
        );
    }

    body.into_any_element()
}

fn render_commit_rules_body(
    form: &GitRulesForm,
    errors: &[RuleFieldError],
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let task_kind_rows = form.type_by_task_kind.iter().map(|(kind, entity)| {
        let error_key = format!("message.typeByTaskKind.{kind}");
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(div().w(gpui::px(110.0)).text_xs().child(kind.clone()))
                    .child(sized_input(entity.clone(), INPUT_SHORT)),
            )
            .when_some(field_error_element(errors, &error_key), |col, err| {
                col.child(err)
            })
    });

    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .child(field_block(
                    "Message format",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-format-conventional",
                            "Conventional",
                            form.format == MessageFormat::Conventional,
                            !busy,
                            cx,
                            |model, cx| {
                                model
                                    .update_git_form(cx, |f| f.format = MessageFormat::Conventional)
                            },
                        ))
                        .child(choice_button(
                            "git-format-free",
                            "Free",
                            form.format == MessageFormat::Free,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| f.format = MessageFormat::Free)
                            },
                        ))
                        .into_any_element(),
                    "message.format",
                    errors,
                ))
                .child(field_block(
                    "Scope",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-scope-slug",
                            "Feature slug",
                            form.scope == MessageScope::FeatureSlug,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| f.scope = MessageScope::FeatureSlug)
                            },
                        ))
                        .child(choice_button(
                            "git-scope-none",
                            "None",
                            form.scope == MessageScope::None,
                            !busy,
                            cx,
                            |model, cx| model.update_git_form(cx, |f| f.scope = MessageScope::None),
                        ))
                        .into_any_element(),
                    "message.scope",
                    errors,
                ))
                .child(field_block(
                    "Commit granularity",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-granularity-task",
                            "Per task",
                            form.granularity == CommitGranularity::PerTask,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| {
                                    f.granularity = CommitGranularity::PerTask
                                })
                            },
                        ))
                        .child(choice_button(
                            "git-granularity-pulsation",
                            "Per pulsation",
                            form.granularity == CommitGranularity::PerPulsation,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| {
                                    f.granularity = CommitGranularity::PerPulsation
                                })
                            },
                        ))
                        .into_any_element(),
                    "granularity",
                    errors,
                )),
        )
        .child(field_block(
            "Allowed commit types (comma separated)",
            sized_input(form.types.clone(), INPUT_LONG).into_any_element(),
            "message.types",
            errors,
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(field_label("Default commit type per task kind"))
                .children(task_kind_rows),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .child(field_block(
                    "Subject max chars (20-200)",
                    sized_input(form.subject_max_chars.clone(), INPUT_SHORT).into_any_element(),
                    "message.subjectMaxChars",
                    errors,
                ))
                .child(field_block(
                    "Body wrap width (40-200)",
                    sized_input(form.body_wrap.clone(), INPUT_SHORT).into_any_element(),
                    "message.bodyWrap",
                    errors,
                ))
                .child(field_block(
                    "Message language (fixed in v1)",
                    readout("en"),
                    "message.language",
                    errors,
                )),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .child(field_block(
                    "Commit body",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-body-required",
                            "Required",
                            form.body_required,
                            !busy,
                            cx,
                            |model, cx| model.update_git_form(cx, |f| f.body_required = true),
                        ))
                        .child(choice_button(
                            "git-body-optional",
                            "Optional",
                            !form.body_required,
                            !busy,
                            cx,
                            |model, cx| model.update_git_form(cx, |f| f.body_required = false),
                        ))
                        .into_any_element(),
                    "message.bodyRequired",
                    errors,
                ))
                .child(field_block(
                    "Ikore trailers",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-trailers-on",
                            "Include",
                            form.trailers,
                            !busy,
                            cx,
                            |model, cx| model.update_git_form(cx, |f| f.trailers = true),
                        ))
                        .child(choice_button(
                            "git-trailers-off",
                            "Omit",
                            !form.trailers,
                            !busy,
                            cx,
                            |model, cx| model.update_git_form(cx, |f| f.trailers = false),
                        ))
                        .into_any_element(),
                    "message.trailers",
                    errors,
                ))
                .child(field_block(
                    "Signed-off-by trailer",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-signoff-on",
                            "Enabled",
                            form.sign_off,
                            !busy,
                            cx,
                            |model, cx| model.update_git_form(cx, |f| f.sign_off = true),
                        ))
                        .child(choice_button(
                            "git-signoff-off",
                            "Disabled",
                            !form.sign_off,
                            !busy,
                            cx,
                            |model, cx| model.update_git_form(cx, |f| f.sign_off = false),
                        ))
                        .into_any_element(),
                    "message.signOff",
                    errors,
                )),
        )
        .child(field_block(
            "Optional subject/message regex pattern (max 500 chars)",
            sized_input(form.pattern.clone(), INPUT_LONG).into_any_element(),
            "message.pattern",
            errors,
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .child(field_block(
                    "Max changed lines (1-50000)",
                    sized_input(form.max_changed_lines.clone(), INPUT_SHORT).into_any_element(),
                    "limits.maxChangedLines",
                    errors,
                ))
                .child(field_block(
                    "Max files (1-1000)",
                    sized_input(form.max_files.clone(), INPUT_SHORT).into_any_element(),
                    "limits.maxFiles",
                    errors,
                ))
                .child(field_block(
                    "When limits exceed",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-exceed-block",
                            "Block",
                            form.on_exceed == RuleAction::Block,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| f.on_exceed = RuleAction::Block)
                            },
                        ))
                        .child(choice_button(
                            "git-exceed-warn",
                            "Warn",
                            form.on_exceed == RuleAction::Warn,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| f.on_exceed = RuleAction::Warn)
                            },
                        ))
                        .into_any_element(),
                    "limits.onExceed",
                    errors,
                )),
        )
        .child(field_block(
            "Exclude from line and file counts (comma separated)",
            sized_input(form.exclude_from_count.clone(), INPUT_LONG).into_any_element(),
            "limits.excludeFromCount",
            errors,
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .child(field_block(
                    "Documentation files placement",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-docs-separate",
                            "Separate commit",
                            form.docs_placement == DocsPlacement::SeparateCommit,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| {
                                    f.docs_placement = DocsPlacement::SeparateCommit
                                })
                            },
                        ))
                        .child(choice_button(
                            "git-docs-with-tasks",
                            "With tasks",
                            form.docs_placement == DocsPlacement::WithTasks,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| {
                                    f.docs_placement = DocsPlacement::WithTasks
                                })
                            },
                        ))
                        .into_any_element(),
                    "docsPlacement",
                    errors,
                ))
                .child(field_block(
                    "Code task without test changes",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-missing-tests-block",
                            "Block",
                            form.missing_tests == RuleAction::Block,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| f.missing_tests = RuleAction::Block)
                            },
                        ))
                        .child(choice_button(
                            "git-missing-tests-warn",
                            "Warn",
                            form.missing_tests == RuleAction::Warn,
                            !busy,
                            cx,
                            |model, cx| {
                                model.update_git_form(cx, |f| f.missing_tests = RuleAction::Warn)
                            },
                        ))
                        .into_any_element(),
                    "consistency.missingTests",
                    errors,
                )),
        )
        .child(field_block(
            "Forbidden paths and globs (comma separated)",
            sized_input(form.forbidden_paths.clone(), INPUT_LONG).into_any_element(),
            "paths.forbidden",
            errors,
        ))
        .into_any_element()
}

fn render_push_pull_body(
    state: &GitSettingsState,
    form: &GitRulesForm,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let errors = &state.field_errors;
    let action_row = div()
        .pt_2()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .child(btn(
            "git-rules-save",
            if state.saving {
                "Saving..."
            } else {
                "Save rules"
            },
            ButtonVariant::Primary,
            !busy && !state.saving,
            cx,
            |model, cx| model.save_git_rules(cx),
        ))
        .child(btn(
            "git-rules-restore",
            "Restore defaults",
            ButtonVariant::Secondary,
            !busy && !state.saving,
            cx,
            |model, cx| model.restore_default_git_rules(cx),
        ))
        .when_some(state.notice.as_ref(), |bar, text| {
            bar.child(
                div()
                    .debug_selector(|| "git-rules-notice".into())
                    .text_xs()
                    .text_color(theme::success())
                    .child(text.clone()),
            )
        })
        .when_some(state.error.as_ref(), |bar, text| {
            bar.child(
                div()
                    .debug_selector(|| "git-rules-error".into())
                    .text_xs()
                    .text_color(theme::danger())
                    .child(text.clone()),
            )
        });

    let mut content = div()
        .flex()
        .flex_col()
        .gap_4()
        .child(field_block(
            "Branch mode",
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(choice_button(
                    "git-branch-per-pulsation",
                    "New branch per pulsation",
                    form.branch_mode == BranchMode::PerPulsation,
                    !busy,
                    cx,
                    |model, cx| {
                        model.update_git_form(cx, |f| f.branch_mode = BranchMode::PerPulsation)
                    },
                ))
                .child(choice_button(
                    "git-branch-current",
                    "Current branch",
                    form.branch_mode == BranchMode::CurrentBranch,
                    !busy,
                    cx,
                    |model, cx| {
                        model.update_git_form(cx, |f| f.branch_mode = BranchMode::CurrentBranch)
                    },
                ))
                .into_any_element(),
            "branch.mode",
            errors,
        ))
        .child(field_block(
            "Branch name pattern ({pulsationShort}, {slug}, {date})",
            sized_input(form.branch_pattern.clone(), INPUT_LONG).into_any_element(),
            "branch.pattern",
            errors,
        ))
        .child(field_block(
            "Protected branches (comma separated)",
            sized_input(form.protected_branches.clone(), INPUT_LONG).into_any_element(),
            "branch.protected",
            errors,
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .child(field_block(
                    "Push remote",
                    sized_input(form.push_remote.clone(), INPUT_MEDIUM).into_any_element(),
                    "push.remote",
                    errors,
                ))
                .child(field_block(
                    "Set upstream on first push",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(choice_button(
                            "git-push-upstream-yes",
                            "Yes",
                            form.set_upstream,
                            !busy,
                            cx,
                            |model, cx| model.update_git_form(cx, |f| f.set_upstream = true),
                        ))
                        .child(choice_button(
                            "git-push-upstream-no",
                            "No",
                            !form.set_upstream,
                            !busy,
                            cx,
                            |model, cx| model.update_git_form(cx, |f| f.set_upstream = false),
                        ))
                        .into_any_element(),
                    "push.setUpstream",
                    errors,
                )),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .child(field_block(
                    "Pull remote",
                    sized_input(form.pull_remote.clone(), INPUT_MEDIUM).into_any_element(),
                    "pull.remote",
                    errors,
                ))
                .child(field_block(
                    "Pull strategy (fixed in v1)",
                    readout("ff_only"),
                    "pull.mode",
                    errors,
                )),
        )
        .child(action_row);

    if !errors.is_empty() {
        content = content.child(
            div()
                .debug_selector(|| "git-validation-summary".into())
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(theme::danger())
                .bg(theme::danger_soft())
                .flex()
                .flex_col()
                .gap_1()
                .children(errors.iter().map(|err| {
                    div()
                        .text_xs()
                        .text_color(theme::danger())
                        .child(format!("{}: {}", err.field, err.message))
                })),
        );
    }

    content.into_any_element()
}

fn render_fixed_rules_body() -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(fixed_git_rules().iter().map(|rule| {
            div()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme::border())
                .bg(theme::canvas())
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(rule.title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(rule.description),
                )
        }))
        .into_any_element()
}

pub(crate) fn git_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let Some(project_id) = model.active_git_project_id(cx) else {
        return no_project();
    };
    let (busy, git, project_name) = {
        let settings = model.settings.read(cx);
        let project_name = settings
            .snapshot
            .as_ref()
            .and_then(|snap| snap.composition.as_ref())
            .filter(|comp| comp.project_id == project_id)
            .map(|comp| comp.project_name.clone())
            .unwrap_or_else(|| project_id.clone());
        (settings.busy, settings.git.clone(), project_name)
    };
    let git = &git;

    let enabled = git
        .form
        .as_ref()
        .map(|form| form.enabled)
        .or_else(|| git.record.as_ref().map(|record| record.enabled))
        .unwrap_or(false);
    let version_note = match git.record.as_ref() {
        Some(record) if record.version > 0 => {
            let short_hash: String = record.rules_sha256.chars().take(12).collect();
            format!("Saved version {} (sha256 {short_hash})", record.version)
        }
        _ => "Using default rules (not saved yet)".to_owned(),
    };

    let commit_body = if let Some(form) = git.form.as_ref() {
        render_commit_rules_body(form, &git.field_errors, busy || git.saving, cx)
    } else {
        hint("Preparing rule fields...")
    };
    let push_pull_body = if let Some(form) = git.form.as_ref() {
        render_push_pull_body(git, form, busy || git.saving, cx)
    } else {
        hint("Preparing push and pull fields...")
    };

    PageContent::new(SettingsPage::Git.label())
        .description(SettingsPage::Git.description())
        .meta(format!("Project · {project_name} · {version_note}"))
        .section(
            "Git tool",
            section(
                "Git tool",
                None,
                vec![row(
                    "git-tool-enabled",
                    hl,
                    switch(
                        "git-tool-enabled",
                        enabled,
                        !busy && !git.saving,
                        cx,
                        |model, cx| model.set_git_tool_enabled(cx),
                    ),
                )],
            ),
        )
        .section(
            "Status",
            section(
                "Status",
                None,
                vec![stacked_row(
                    "git-status",
                    hl,
                    render_status_body(git, busy, cx),
                )],
            ),
        )
        .section(
            "Commit rules",
            section(
                "Commit rules",
                None,
                vec![stacked_row("git-commit-rules", hl, commit_body)],
            ),
        )
        .section(
            "Push and pull rules",
            section(
                "Push and pull rules",
                None,
                vec![stacked_row("git-push-pull-rules", hl, push_pull_body)],
            ),
        )
        .section(
            "Fixed host rules",
            section(
                "Fixed host rules",
                None,
                vec![stacked_row(
                    "git-fixed-rules",
                    hl,
                    render_fixed_rules_body(),
                )],
            ),
        )
}
