//! Settings › MCP servers (ADR-043): the local MCP servers the host runs, the
//! tools each lists and which of them the Companion may call in this project.
//! Pipeline agents and free-form slots keep their fixed profile: no MCP.

use gpui::{AnyElement, Context, Entity, SharedString, div, prelude::*, px};
use ikore_core::mcp::{McpServerView, SaveMcpServerRequest};

use crate::app_model::AppModel;
use crate::settings::SettingsPage;
use crate::settings_ui::{PageContent, note, section, stacked_row};
use crate::settings_view::action_with;
use crate::text_input::NativeTextInput;
use crate::theme;
use crate::widgets::ButtonVariant;

/// The page's own state, beside the settings snapshot.
#[derive(Debug, Default)]
pub(crate) struct McpSettings {
    pub(crate) servers: Vec<McpServerView>,
    /// The project the list belongs to.
    pub(crate) project_id: Option<String>,
    pub(crate) loading: bool,
    /// What runs now: `save`, or the id of the server being connected.
    pub(crate) busy: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) form: Option<McpForm>,
    /// The server whose deletion waits for a second click.
    pub(crate) delete_armed: Option<String>,
}

/// The add or edit form.
#[derive(Debug, Clone)]
pub(crate) struct McpForm {
    pub(crate) id: Option<String>,
    pub(crate) version: Option<u64>,
    /// Every project sees a global server.
    pub(crate) global: bool,
    pub(crate) name: Entity<NativeTextInput>,
    pub(crate) command: Entity<NativeTextInput>,
    pub(crate) args: Entity<NativeTextInput>,
    pub(crate) variable: Entity<NativeTextInput>,
    pub(crate) value: Entity<NativeTextInput>,
    /// Each variable with the value typed here, or `None` to keep the stored one.
    pub(crate) env: Vec<(String, Option<String>)>,
}

/// Splits the arguments line on spaces; double quotes keep spaces inside one.
pub(crate) fn split_args(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in line.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            other => {
                current.push(other);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// The arguments as one line, quoting those with spaces.
fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.contains(' ') || arg.is_empty() {
                format!("\"{arg}\"")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl AppModel {
    fn mcp_project(&self, cx: &gpui::App) -> Option<String> {
        self.workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
    }

    /// Reads the servers the open project sees.
    pub(crate) fn refresh_mcp_servers(&mut self, cx: &mut Context<Self>) {
        let (Some(backend), Some(project_id)) =
            (self.settings_backend.clone(), self.mcp_project(cx))
        else {
            return;
        };
        self.settings.update(cx, |settings, _| {
            settings.mcp.loading = true;
            settings.mcp.project_id = Some(project_id.clone());
        });
        cx.spawn(async move |this, cx| {
            let result = backend.mcp_servers(project_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                model.settings.update(cx, |settings, _| {
                    if settings.mcp.project_id.as_deref() != Some(project_id.as_str()) {
                        return;
                    }
                    settings.mcp.loading = false;
                    match result {
                        Ok(servers) => settings.mcp.servers = servers,
                        Err(error) => settings.mcp.error = Some(error.message),
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs one change, then reads the list again.
    fn run_mcp(
        &mut self,
        busy: String,
        operation: crate::settings::SettingsFuture<()>,
        cx: &mut Context<Self>,
    ) {
        self.settings.update(cx, |settings, _| {
            settings.mcp.busy = Some(busy);
            settings.mcp.error = None;
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |model, cx| {
                model.settings.update(cx, |settings, _| {
                    settings.mcp.busy = None;
                    if let Err(error) = result {
                        settings.mcp.error = Some(error.message);
                    }
                });
                model.refresh_mcp_servers(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the form, empty or on `server_id`.
    pub(crate) fn open_mcp_form(&mut self, server_id: Option<String>, cx: &mut Context<Self>) {
        let server = server_id.and_then(|id| {
            self.settings
                .read(cx)
                .mcp
                .servers
                .iter()
                .find(|server| server.id == id)
                .cloned()
        });
        let input = |placeholder: &str, value: String, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = NativeTextInput::new(placeholder.to_owned(), cx);
                input.set_value(value, cx);
                input
            })
        };
        let form = McpForm {
            id: server.as_ref().map(|server| server.id.clone()),
            version: server.as_ref().map(|server| server.version),
            global: server
                .as_ref()
                .is_some_and(|server| server.project_id.is_none()),
            name: input(
                "Server name, as in mcp:<name>/<tool>",
                server
                    .as_ref()
                    .map(|server| server.name.clone())
                    .unwrap_or_default(),
                cx,
            ),
            command: input(
                "Command (npx, uvx, a path)",
                server
                    .as_ref()
                    .map(|server| server.command.clone())
                    .unwrap_or_default(),
                cx,
            ),
            args: input(
                "Arguments",
                server
                    .as_ref()
                    .map(|server| join_args(&server.args))
                    .unwrap_or_default(),
                cx,
            ),
            variable: input("VARIABLE_NAME", String::new(), cx),
            value: cx.new(|cx| NativeTextInput::new("Value (not displayed again)", cx).obscured()),
            env: server
                .as_ref()
                .map(|server| {
                    server
                        .env_names
                        .iter()
                        .map(|name| (name.clone(), None))
                        .collect()
                })
                .unwrap_or_default(),
        };
        self.settings.update(cx, |settings, _| {
            settings.mcp.form = Some(form);
            settings.mcp.error = None;
        });
        cx.notify();
    }

    pub(crate) fn cancel_mcp_form(&mut self, cx: &mut Context<Self>) {
        self.settings
            .update(cx, |settings, _| settings.mcp.form = None);
        cx.notify();
    }

    pub(crate) fn toggle_mcp_form_scope(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            if let Some(form) = settings.mcp.form.as_mut().filter(|form| form.id.is_none()) {
                form.global = !form.global;
            }
        });
        cx.notify();
    }

    /// Adds the typed variable, or replaces its value.
    pub(crate) fn add_mcp_variable(&mut self, cx: &mut Context<Self>) {
        let Some((variable, value)) = self
            .settings
            .read(cx)
            .mcp
            .form
            .as_ref()
            .map(|form| (form.variable.clone(), form.value.clone()))
        else {
            return;
        };
        let name = variable.read(cx).value().trim().to_owned();
        let secret = value.read(cx).value().to_owned();
        if name.is_empty() || secret.is_empty() {
            return;
        }
        self.settings.update(cx, |settings, _| {
            if let Some(form) = settings.mcp.form.as_mut() {
                form.env.retain(|(existing, _)| existing != &name);
                form.env.push((name, Some(secret)));
            }
        });
        variable.update(cx, |input, cx| input.clear(cx));
        value.update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    pub(crate) fn remove_mcp_variable(&mut self, name: String, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            if let Some(form) = settings.mcp.form.as_mut() {
                form.env.retain(|(existing, _)| existing != &name);
            }
        });
        cx.notify();
    }

    pub(crate) fn save_mcp_server(&mut self, cx: &mut Context<Self>) {
        let (Some(backend), Some(project_id)) =
            (self.settings_backend.clone(), self.mcp_project(cx))
        else {
            return;
        };
        let request = {
            let settings = self.settings.read(cx);
            let Some(form) = settings.mcp.form.as_ref() else {
                return;
            };
            SaveMcpServerRequest {
                id: form.id.clone(),
                project_id: (!form.global).then(|| project_id.clone()),
                name: form.name.read(cx).value().trim().to_owned(),
                command: form.command.read(cx).value().trim().to_owned(),
                args: split_args(form.args.read(cx).value()),
                env: form.env.clone(),
                enabled: true,
                expected_version: form.version,
            }
        };
        self.settings
            .update(cx, |settings, _| settings.mcp.form = None);
        self.run_mcp(
            "save".into(),
            Box::pin(async move { backend.save_mcp_server(request).await.map(|_| ()) }),
            cx,
        );
    }

    /// "Test connection": the server starts, lists its tools and stops.
    pub(crate) fn connect_mcp_server(&mut self, server_id: String, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let id = server_id.clone();
        self.run_mcp(
            server_id,
            Box::pin(async move { backend.connect_mcp_server(id).await }),
            cx,
        );
    }

    /// Deletes on the second click.
    pub(crate) fn request_delete_mcp_server(&mut self, server_id: String, cx: &mut Context<Self>) {
        let armed = self.settings.read(cx).mcp.delete_armed.as_deref() == Some(server_id.as_str());
        if !armed {
            self.settings.update(cx, |settings, _| {
                settings.mcp.delete_armed = Some(server_id);
            });
            cx.notify();
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        self.settings
            .update(cx, |settings, _| settings.mcp.delete_armed = None);
        let id = server_id.clone();
        self.run_mcp(
            server_id,
            Box::pin(async move { backend.delete_mcp_server(id).await }),
            cx,
        );
    }

    pub(crate) fn set_mcp_grant(
        &mut self,
        server_id: String,
        tool: String,
        granted: bool,
        cx: &mut Context<Self>,
    ) {
        let (Some(backend), Some(project_id)) =
            (self.settings_backend.clone(), self.mcp_project(cx))
        else {
            return;
        };
        let id = server_id.clone();
        self.run_mcp(
            server_id,
            Box::pin(async move { backend.set_mcp_grant(project_id, id, tool, granted).await }),
            cx,
        );
    }
}

/// A settings button whose id and label are built per server or tool.
fn button(
    id: String,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let selector = id.clone();
    crate::widgets::button_frame(
        div()
            .id(SharedString::from(id))
            .debug_selector(move || selector.clone()),
        variant,
        enabled,
    )
    .when(enabled, |button| {
        button.on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
    })
    .child(label.into())
    .into_any_element()
}

fn label(text: &'static str) -> AnyElement {
    div()
        .text_xs()
        .text_color(theme::text_muted())
        .child(text)
        .into_any_element()
}

fn field(title: &'static str, input: Entity<NativeTextInput>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(label(title))
        .child(crate::sizing::sized_input(input, crate::sizing::INPUT_LONG))
        .into_any_element()
}

fn badge(text: impl Into<SharedString>, color: gpui::Rgba) -> AnyElement {
    div()
        .px_1()
        .rounded_sm()
        .border_1()
        .border_color(color)
        .text_xs()
        .text_color(color)
        .child(text.into())
        .into_any_element()
}

fn server_card(
    server: &McpServerView,
    busy: Option<&str>,
    delete_armed: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = server.id.clone();
    let working = busy.is_some();
    let connecting = busy == Some(server.id.as_str());
    let state = match (&server.last_error, &server.connected_at) {
        (Some(error), _) => (theme::danger(), error.clone()),
        (None, Some(_)) => (
            theme::success(),
            format!("Connected · {} tool(s)", server.tools.len()),
        ),
        (None, None) => (
            theme::text_muted(),
            "Not connected: nothing runs yet".into(),
        ),
    };
    let tools = server
        .tools
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            let selector = format!("mcp-tool:{}:{index}", server.id);
            let server_id = server.id.clone();
            let name = tool.name.clone();
            let granted = tool.granted_to_companion;
            div()
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .font_family(theme::FONT_MONO)
                                .child(tool.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .truncate()
                                .child(tool.description.clone()),
                        ),
                )
                .child(if tool.read_only {
                    badge("read-only", theme::text_muted())
                } else {
                    badge("may change things", theme::warning())
                })
                .child(button(
                    format!("mcp-grant:{}:{index}", server.id),
                    if granted {
                        "Companion: allowed"
                    } else {
                        "Allow for the Companion"
                    },
                    if granted {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Ghost
                    },
                    !working && tool.read_only,
                    cx,
                    move |model, cx| {
                        model.set_mcp_grant(server_id.clone(), name.clone(), !granted, cx)
                    },
                ))
                .into_any_element()
        })
        .collect::<Vec<_>>();
    let edit_id = server.id.clone();
    let delete_id = server.id.clone();
    let selector = format!("mcp-server:{}", server.id);
    div()
        .debug_selector(move || selector.clone())
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(server.name.clone()),
                )
                .child(badge(
                    if server.project_id.is_some() {
                        "this project"
                    } else {
                        "all projects"
                    },
                    theme::text_muted(),
                ))
                .when(server.overridden, |row| {
                    row.child(badge("hidden by this project's server", theme::warning()))
                })
                .child(div().flex_1())
                .child(button(
                    format!("mcp-connect:{id}"),
                    if connecting {
                        "Connecting…"
                    } else {
                        "Test connection"
                    },
                    ButtonVariant::Secondary,
                    !working,
                    cx,
                    move |model, cx| model.connect_mcp_server(id.clone(), cx),
                ))
                .child(button(
                    format!("mcp-edit:{edit_id}"),
                    "Edit",
                    ButtonVariant::Ghost,
                    !working,
                    cx,
                    move |model, cx| model.open_mcp_form(Some(edit_id.clone()), cx),
                ))
                .child(button(
                    format!("mcp-delete:{delete_id}"),
                    if delete_armed {
                        "Confirm delete"
                    } else {
                        "Delete"
                    },
                    ButtonVariant::Danger,
                    !working,
                    cx,
                    move |model, cx| model.request_delete_mcp_server(delete_id.clone(), cx),
                )),
        )
        .child(
            div()
                .text_xs()
                .font_family(theme::FONT_MONO)
                .text_color(theme::text_muted())
                .truncate()
                .child(format!("{} {}", server.command, join_args(&server.args))),
        )
        .when(!server.env_names.is_empty(), |card| {
            card.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!("Variables: {}", server.env_names.join(", "))),
            )
        })
        .child(div().text_xs().text_color(state.0).child(state.1))
        .children(tools)
        .into_any_element()
}

fn form_body(form: &McpForm, busy: bool, cx: &mut Context<AppModel>) -> AnyElement {
    let variables = form.env.iter().map(|(name, value)| {
        let remove = name.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().text_sm().font_family(theme::FONT_MONO).child(format!(
                "{name} = {}",
                if value.is_some() {
                    "•••• (new)"
                } else {
                    "•••• (stored)"
                }
            )))
            .child(button(
                format!("mcp-remove-variable:{name}"),
                "Remove",
                ButtonVariant::Ghost,
                true,
                cx,
                move |model, cx| model.remove_mcp_variable(remove.clone(), cx),
            ))
            .into_any_element()
    });
    div()
        .id("mcp-form")
        .debug_selector(|| "mcp-form".into())
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::accent())
        .flex()
        .flex_col()
        .gap_3()
        .child(field("NAME", form.name.clone()))
        .child(field("COMMAND", form.command.clone()))
        .child(field("ARGUMENTS", form.args.clone()))
        .child(label(
            "VARIABLES · values stay in the system's credential store",
        ))
        .children(variables)
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_end()
                .gap_2()
                .child(crate::sizing::sized_input(
                    form.variable.clone(),
                    crate::sizing::INPUT_MEDIUM,
                ))
                .child(crate::sizing::sized_input(
                    form.value.clone(),
                    crate::sizing::INPUT_MEDIUM,
                ))
                .child(action_with(
                    "mcp-add-variable",
                    "Add variable",
                    ButtonVariant::Ghost,
                    true,
                    cx,
                    |model, cx| model.add_mcp_variable(cx),
                )),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .when(form.id.is_none(), |row| {
                    row.child(action_with(
                        "mcp-scope",
                        if form.global {
                            "For all projects"
                        } else {
                            "For this project"
                        },
                        ButtonVariant::Ghost,
                        true,
                        cx,
                        |model, cx| model.toggle_mcp_form_scope(cx),
                    ))
                })
                .child(div().flex_1())
                .child(action_with(
                    "mcp-cancel",
                    "Cancel",
                    ButtonVariant::Ghost,
                    true,
                    cx,
                    |model, cx| model.cancel_mcp_form(cx),
                ))
                .child(action_with(
                    "mcp-save",
                    "Save",
                    ButtonVariant::Primary,
                    !busy,
                    cx,
                    |model, cx| model.save_mcp_server(cx),
                )),
        )
        .into_any_element()
}

/// Settings › MCP servers.
pub(crate) fn mcp_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let project = model
        .workspace
        .read(cx)
        .project_id
        .as_ref()
        .map(ToString::to_string);
    let (servers, busy, error, loading, loaded_for, delete_armed) = {
        let mcp = &model.settings.read(cx).mcp;
        (
            mcp.servers.clone(),
            mcp.busy.clone(),
            mcp.error.clone(),
            mcp.loading,
            mcp.project_id.clone(),
            mcp.delete_armed.clone(),
        )
    };
    // The list is read when the page first shows for a project.
    if project.is_some() && loaded_for != project && !loading {
        let app = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = app.update(cx, |model, cx| model.refresh_mcp_servers(cx));
        });
    }
    // Input handles and names: cheap to clone, and the form renders with `cx` free.
    let form = model.settings.read(cx).mcp.form.clone();
    let mut list: Vec<AnyElement> = Vec::new();
    if let Some(error) = error {
        list.push(
            div()
                .text_sm()
                .text_color(theme::danger())
                .child(error)
                .into_any_element(),
        );
    }
    if project.is_none() {
        list.push(note("Open a project to manage its MCP servers."));
    } else if servers.is_empty() {
        list.push(note(if loading {
            "Reading the servers…"
        } else {
            "No MCP server yet. The host runs one only when you test it or the Companion calls a tool you allowed."
        }));
    }
    for server in &servers {
        list.push(server_card(
            server,
            busy.as_deref(),
            delete_armed.as_deref() == Some(server.id.as_str()),
            cx,
        ));
    }
    let form_element = form
        .as_ref()
        .map(|form| form_body(form, busy.is_some(), cx));
    let add = action_with(
        "mcp-add",
        "Add server",
        ButtonVariant::Secondary,
        project.is_some() && form.is_none(),
        cx,
        |model, cx| model.open_mcp_form(None, cx),
    );
    PageContent::new(SettingsPage::Mcp.label())
        .description(SettingsPage::Mcp.description())
        .section(
            "Servers",
            section(
                "Servers",
                None,
                vec![stacked_row(
                    "mcp-servers",
                    hl,
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .children(list)
                        .children(form_element)
                        .child(div().child(add)),
                )],
            ),
        )
        .section(
            "Pipeline",
            section(
                "Pipeline",
                None,
                vec![stacked_row(
                    "mcp-pipeline",
                    hl,
                    note("Pipeline agents and free-form slots keep their fixed profile: the CLI gets no MCP server, from you or from the host. Only the Companion can be allowed tools here, and only read-only ones."),
                )],
            ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Arguments split on spaces; quotes keep a space inside one; the line
    /// round-trips through the form.
    #[test]
    fn arguments_split_on_spaces_and_keep_quoted_ones() {
        assert_eq!(
            split_args(r#"-y @scope/server  --root "C:\My Docs" """#),
            ["-y", "@scope/server", "--root", r"C:\My Docs", ""]
        );
        assert!(split_args("   ").is_empty());
        let args = split_args(r#"--name "two words" plain"#);
        assert_eq!(split_args(&join_args(&args)), args);
    }
}
