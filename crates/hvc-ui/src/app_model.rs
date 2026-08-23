use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, PathPromptOptions, SharedString, Window};
use hvc_core::persistence_service::{
    EnsureProjectForWorkspaceRequest, LoadUiSessionRequest, UiSessionState,
};
use hvc_core::workspace_service::{
    OpenWorkspaceRequest, ReadWorkspaceFileRequest, ReadWorkspaceFileResponse,
};
use hvc_core::{AppCore, AppEvent};
use hvc_editor::{EditorSelection, TextPosition};
use tokio::sync::broadcast::error::RecvError;

use crate::views::{
    ChatModel, LogsModel, NavigationModel, PulsationsModel, SettingsModel, VaultModel,
    WorkspaceEditorModel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppView {
    Workspace,
    Pulsations,
    Vault,
    Chat,
    Settings,
    Logs,
}

impl AppView {
    pub const ALL: [Self; 6] = [
        Self::Workspace,
        Self::Pulsations,
        Self::Vault,
        Self::Chat,
        Self::Settings,
        Self::Logs,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Pulsations => "pulsations",
            Self::Vault => "vault",
            Self::Chat => "chat",
            Self::Settings => "settings",
            Self::Logs => "logs",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Pulsations => "Pulsations",
            Self::Vault => "Vault",
            Self::Chat => "Chat Companion",
            Self::Settings => "Settings",
            Self::Logs => "Logs",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Workspace => "Arquivos, abas, editor, preview e revisão HITL",
            Self::Pulsations => "Execuções, checkpoints e decisões humanas",
            Self::Vault => "Notas Markdown, links e contexto do workspace",
            Self::Chat => "Conversa, streaming e ferramentas read-only",
            Self::Settings => "Provedores, rotas LLM e preferências locais",
            Self::Logs => "Eventos tipados e diagnósticos do runtime",
        }
    }

    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|view| view.id() == id)
            .unwrap_or(Self::Workspace)
    }
}

struct RestoredSession {
    project_id: String,
    root_path: String,
    session: Option<UiSessionState>,
    active_file: Option<ReadWorkspaceFileResponse>,
}

pub struct AppModel {
    core: Option<Arc<AppCore>>,
    pub navigation: Entity<NavigationModel>,
    pub workspace: Entity<WorkspaceEditorModel>,
    pub pulsations: Entity<PulsationsModel>,
    pub vault: Entity<VaultModel>,
    pub chat: Entity<ChatModel>,
    pub settings: Entity<SettingsModel>,
    pub logs: Entity<LogsModel>,
    active_view: AppView,
    status: SharedString,
    event_count: u64,
    needs_resync: bool,
}

impl AppModel {
    pub fn new(core: Arc<AppCore>, cx: &mut Context<Self>) -> Self {
        let mut receiver = core.subscribe();
        cx.spawn(async move |this, cx| {
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        if this
                            .update(cx, |model, cx| model.handle_event(event, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        if this
                            .update(cx, |model, cx| {
                                model.needs_resync = true;
                                model.status =
                                    "Eventos atrasados; recarregando snapshot persistido".into();
                                model.restore_last_session(cx);
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        })
        .detach();

        let mut model = Self::build(Some(core), cx);
        model.restore_last_session(cx);
        model
    }

    #[cfg(test)]
    pub fn new_for_test(cx: &mut Context<Self>) -> Self {
        Self::build(None, cx)
    }

    fn build(core: Option<Arc<AppCore>>, cx: &mut Context<Self>) -> Self {
        let active_view = AppView::Workspace;
        Self {
            core,
            navigation: cx.new(|_| NavigationModel {
                active: active_view,
            }),
            workspace: cx.new(WorkspaceEditorModel::new),
            pulsations: cx.new(|_| PulsationsModel::default()),
            vault: cx.new(|_| VaultModel::default()),
            chat: cx.new(|_| ChatModel::default()),
            settings: cx.new(|_| SettingsModel::default()),
            logs: cx.new(|_| LogsModel::default()),
            active_view,
            status: "Pronto".into(),
            event_count: 0,
            needs_resync: false,
        }
    }

    pub fn active_view(&self) -> AppView {
        self.active_view
    }

    pub fn status(&self) -> &SharedString {
        &self.status
    }

    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    pub fn needs_resync(&self) -> bool {
        self.needs_resync
    }

    pub fn select_view(&mut self, view: AppView, cx: &mut Context<Self>) {
        self.active_view = view;
        self.navigation.update(cx, |navigation, _| {
            navigation.active = view;
        });
        self.status = format!("View {} ativa", view.label()).into();
        cx.notify();
    }

    pub fn choose_workspace(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            self.status = "Seletor indisponível no ambiente de teste".into();
            cx.notify();
            return;
        };

        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Selecionar workspace".into()),
        });
        self.status = "Aguardando seleção do workspace…".into();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let path = match selected.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(path) = path else {
                let _ = this.update(cx, |model, cx| {
                    model.status = "Seleção de workspace cancelada".into();
                    cx.notify();
                });
                return;
            };

            let request = OpenWorkspaceRequest {
                path: path.to_string_lossy().into_owned(),
            };
            let worker_core = Arc::clone(&core);
            let opened = core
                .runtime_handle()
                .spawn_blocking(move || {
                    let opened = worker_core.open_workspace(request)?;
                    let project = worker_core
                        .persistence()
                        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                            workspace_path: opened.root_path.clone(),
                            preferred_name: None,
                        })
                        .map_err(|error| hvc_core::workspace_service::WorkspaceCommandError {
                            code: hvc_core::workspace_service::WorkspaceErrorCode::IoError,
                            message: error.to_string(),
                            details: None,
                            remediation: None,
                        })?
                        .project;
                    Ok::<_, hvc_core::workspace_service::WorkspaceCommandError>((opened, project))
                })
                .await;

            let _ = this.update(cx, |model, cx| {
                match opened {
                    Ok(Ok((response, project))) => {
                        let root: SharedString = response.root_path.into();
                        model.workspace.update(cx, |workspace, _| {
                            workspace.project_id = Some(project.id.into());
                            workspace.root_path = Some(root.clone());
                            workspace.active_relative_path = None;
                            workspace.open_tabs = 0;
                        });
                        model.needs_resync = false;
                        model.status = format!("Workspace aberto: {root}").into();
                    }
                    Ok(Err(error)) => model.status = error.message.into(),
                    Err(error) => {
                        model.status = format!("Falha no worker de filesystem: {error}").into()
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn restore_last_session(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            cx.notify();
            return;
        };
        self.status = "Restaurando sessão persistida…".into();
        cx.notify();

        let runtime = core.runtime_handle().clone();
        cx.spawn(async move |this, cx| {
            let restored = runtime
                .spawn_blocking(move || -> Result<Option<RestoredSession>, String> {
                    let Some(project) = core
                        .persistence()
                        .get_last_active_project()
                        .map_err(|error| error.to_string())?
                        .project
                    else {
                        return Ok(None);
                    };
                    let opened = core
                        .open_workspace(OpenWorkspaceRequest {
                            path: project.workspace_path.clone(),
                        })
                        .map_err(|error| error.message)?;
                    let session = core
                        .persistence()
                        .load_ui_session(LoadUiSessionRequest {
                            project_id: project.id.clone(),
                        })
                        .map_err(|error| error.to_string())?
                        .session;
                    let active_file = session
                        .as_ref()
                        .and_then(|session| session.active_tab_path.as_ref())
                        .map(|relative_path| {
                            if let Some(tab) = session.as_ref().and_then(|session| {
                                session
                                    .tabs
                                    .iter()
                                    .find(|tab| &tab.relative_path == relative_path)
                            }) && let Some(draft) = &tab.draft_content
                            {
                                return Ok(ReadWorkspaceFileResponse {
                                    content: draft.clone(),
                                    revision: tab
                                        .base_revision
                                        .clone()
                                        .unwrap_or_else(|| "draft".into()),
                                });
                            }
                            core.read_workspace_file(ReadWorkspaceFileRequest {
                                relative_path: relative_path.clone(),
                            })
                            .map_err(|error| error.message)
                        })
                        .transpose()?;

                    Ok(Some(RestoredSession {
                        project_id: project.id,
                        root_path: opened.root_path,
                        session,
                        active_file,
                    }))
                })
                .await;

            let _ = this.update(cx, |model, cx| {
                match restored {
                    Ok(Ok(Some(restored))) => model.apply_restored_session(restored, cx),
                    Ok(Ok(None)) => {
                        model.needs_resync = false;
                        model.status = "Pronto — selecione um workspace".into();
                    }
                    Ok(Err(error)) => {
                        model.status = format!("Falha ao restaurar sessão: {error}").into();
                    }
                    Err(error) => {
                        model.status = format!("Falha no worker de restauração: {error}").into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_restored_session(&mut self, restored: RestoredSession, cx: &mut Context<Self>) {
        let active_view = restored
            .session
            .as_ref()
            .map(|session| AppView::from_id(&session.active_view))
            .unwrap_or(AppView::Workspace);
        let active_path = restored
            .session
            .as_ref()
            .and_then(|session| session.active_tab_path.clone());
        let open_tabs = restored
            .session
            .as_ref()
            .map_or(0, |session| session.tabs.len());
        let cursor = restored.session.as_ref().and_then(|session| {
            let active = session.active_tab_path.as_ref()?;
            let tab = session
                .tabs
                .iter()
                .find(|tab| &tab.relative_path == active)?;
            Some((
                TextPosition {
                    line: tab.cursor_line.unwrap_or(0),
                    column: tab.cursor_column.unwrap_or(0),
                },
                tab.scroll_top.unwrap_or(0.0) as f32,
            ))
        });

        self.workspace.update(cx, |workspace, cx| {
            workspace.project_id = Some(restored.project_id.into());
            workspace.root_path = Some(restored.root_path.clone().into());
            workspace.active_relative_path = active_path.map(Into::into);
            workspace.open_tabs = open_tabs;
            if let Some(file) = restored.active_file {
                workspace.editor.update(cx, |editor, cx| {
                    editor.buffer.reload(file.content, file.revision);
                    if let Some((position, scroll_top)) = cursor {
                        let _ = editor
                            .buffer
                            .set_selection(EditorSelection::caret(position));
                        editor.buffer.set_scroll_top(scroll_top);
                    }
                    cx.notify();
                });
            }
        });
        self.select_view(active_view, cx);
        self.needs_resync = false;
        self.status = format!("Sessão restaurada: {}", restored.root_path).into();
    }

    fn handle_event(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        self.event_count = self.event_count.saturating_add(1);
        self.status = format!("Evento recebido: {}", event.event_name()).into();
        if matches!(event, AppEvent::WorkspaceChanged(_)) {
            self.needs_resync = true;
        }
        self.logs.update(cx, |logs, _| {
            logs.visible_entries = logs.visible_entries.saturating_add(1);
        });
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn navigates_all_six_views(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        for view in AppView::ALL {
            model.update(cx, |model, cx| model.select_view(view, cx));
            assert_eq!(cx.read(|cx| model.read(cx).active_view()), view);
        }
    }

    #[gpui::test]
    fn creates_specific_entities_for_every_view(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        cx.read(|cx| {
            let model = model.read(cx);
            assert_eq!(model.workspace.read(cx).open_tabs, 0);
            assert_eq!(model.workspace.read(cx).editor.read(cx).buffer.text(), "");
            assert_eq!(model.pulsations.read(cx).running_jobs, 0);
            assert_eq!(model.vault.read(cx).indexed_notes, 0);
            assert!(!model.chat.read(cx).streaming);
            assert_eq!(model.settings.read(cx).configured_providers, 0);
            assert_eq!(model.logs.read(cx).visible_entries, 0);
        });
    }

    #[gpui::test]
    fn unknown_persisted_view_falls_back_to_workspace(cx: &mut TestAppContext) {
        let _ = cx;
        assert_eq!(AppView::from_id("chat"), AppView::Chat);
        assert_eq!(AppView::from_id("future-view"), AppView::Workspace);
    }
}
