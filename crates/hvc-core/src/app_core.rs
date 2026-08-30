use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fs2::FileExt;
use reqwest::Client;
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::chat_service::{ChatEventSink, ChatService};
use crate::embedding_service::{EmbeddingGateway, OllamaEmbeddingGateway};
use crate::events::{AppEvent, AppEventBus};
use crate::llm::{
    Clock, CredentialStore, JitterSource, KeyringCredentialStore, LlmEventSink, StableJitter,
    SystemClock,
};
use crate::orchestration::CollectionBudget;
use crate::orchestration::brownfield::{
    BrownfieldOnboardingResult, BrownfieldOnboardingStatus, InventoryScanner,
    SetBrownfieldConsentRequest,
};
use crate::persistence_service::{
    PersistenceCommandError, PersistenceService, PersistenceServiceError,
};
use crate::workspace_service::{
    self, BootstrapGreenfieldWorkspaceResponse, GetContextCandidatesRequest,
    GetContextCandidatesResponse, GetWorkspaceVaultIndexRequest, GetWorkspaceVaultIndexResponse,
    OpenWorkspaceRequest, OpenWorkspaceResponse, ReadWorkspaceFileRequest,
    ReadWorkspaceFileResponse, RenameWorkspaceNoteRequest, RenameWorkspaceNoteResponse,
    SearchWorkspaceTextRequest, SearchWorkspaceTextResponse, WorkspaceCommandError,
    WorkspaceEventSink, WorkspaceServiceError, WorkspaceTreeResponse, WorkspaceWatcher,
    WriteWorkspaceFileAtomicRequest, WriteWorkspaceFileAtomicResponse,
};

const APP_IDENTIFIER: &str = "com.hardvibecode.desktop";
const DATABASE_NAME: &str = "hard-vibe-code.db";
const LOCK_NAME: &str = "hard-vibe-code.lock";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    data_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> Result<Self, AppCoreError> {
        if let Some(override_dir) = std::env::var_os("HVC_DATA_DIR") {
            return Ok(Self::from_data_dir(override_dir));
        }

        #[cfg(target_os = "windows")]
        let base = std::env::var_os("APPDATA").ok_or(AppCoreError::DataDirectoryUnavailable)?;
        #[cfg(not(target_os = "windows"))]
        let base = std::env::var_os("XDG_DATA_HOME")
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local/share").into_os_string())
            })
            .ok_or(AppCoreError::DataDirectoryUnavailable)?;

        Ok(Self::from_data_dir(
            PathBuf::from(base).join(APP_IDENTIFIER),
        ))
    }

    pub fn from_data_dir(path: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: path.into(),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join(DATABASE_NAME)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppCoreError {
    #[error("não foi possível resolver o diretório de dados da aplicação")]
    DataDirectoryUnavailable,
    #[error("outra instância do Hard Vibe Code já está usando este diretório de dados")]
    DataDirectoryInUse,
    #[error("falha de I/O no diretório de dados: {0}")]
    Io(#[from] std::io::Error),
    #[error("falha ao inicializar persistência: {0}")]
    Persistence(#[from] PersistenceServiceError),
    #[error("falha ao recuperar o Chat Companion: {0}")]
    Chat(#[from] crate::chat_service::ChatCommandError),
}

#[derive(Clone)]
pub struct AppCoreDependencies {
    pub credentials: Arc<dyn CredentialStore>,
    pub embedding_gateway: Arc<dyn EmbeddingGateway>,
    pub clock: Arc<dyn Clock>,
    pub jitter: Arc<dyn JitterSource>,
    pub http_client: Client,
}

impl Default for AppCoreDependencies {
    fn default() -> Self {
        let http_client = Client::new();
        Self {
            credentials: Arc::new(KeyringCredentialStore),
            embedding_gateway: Arc::new(OllamaEmbeddingGateway::new(http_client.clone())),
            clock: Arc::new(SystemClock),
            jitter: Arc::new(StableJitter),
            http_client,
        }
    }
}

#[derive(Debug)]
pub struct DataDirectoryLock {
    file: File,
    lock_path: PathBuf,
}

impl DataDirectoryLock {
    pub fn acquire(paths: &AppPaths) -> Result<Self, AppCoreError> {
        fs::create_dir_all(paths.data_dir())?;
        let lock_path = fs::canonicalize(paths.data_dir())?.join(LOCK_NAME);

        {
            let mut held = process_locks()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !held.insert(lock_path.clone()) {
                return Err(AppCoreError::DataDirectoryInUse);
            }
        }

        let acquired = (|| {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)?;
            file.try_lock_exclusive()
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::WouldBlock => AppCoreError::DataDirectoryInUse,
                    _ => AppCoreError::Io(error),
                })?;
            Ok(Self {
                file,
                lock_path: lock_path.clone(),
            })
        })();

        if acquired.is_err() {
            process_locks()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&lock_path);
        }
        acquired
    }
}

impl Drop for DataDirectoryLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        process_locks()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.lock_path);
    }
}

fn process_locks() -> &'static Mutex<HashSet<PathBuf>> {
    static LOCKS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

#[derive(Default)]
struct WorkspaceContext {
    root_path: Option<PathBuf>,
    watcher: Option<WorkspaceWatcher>,
}

pub struct AppCore {
    paths: AppPaths,
    runtime: Handle,
    _data_lock: DataDirectoryLock,
    events: Arc<AppEventBus>,
    persistence: PersistenceService,
    chat: Arc<ChatService>,
    workspace: Mutex<WorkspaceContext>,
    worker_cancellation: CancellationToken,
    worker_task: Mutex<Option<JoinHandle<()>>>,
}

impl AppCore {
    pub fn initialize(paths: AppPaths, runtime: Handle) -> Result<Arc<Self>, AppCoreError> {
        Self::initialize_with_dependencies(paths, runtime, AppCoreDependencies::default())
    }

    pub fn initialize_with_dependencies(
        paths: AppPaths,
        runtime: Handle,
        dependencies: AppCoreDependencies,
    ) -> Result<Arc<Self>, AppCoreError> {
        let data_lock = DataDirectoryLock::acquire(&paths)?;
        let events = Arc::new(AppEventBus::default());
        let llm_sink: Arc<dyn LlmEventSink> = events.clone();
        let chat_http_client = dependencies.http_client.clone();
        let persistence = PersistenceService::initialize_with_dependencies(
            paths.data_dir(),
            llm_sink,
            dependencies.credentials.clone(),
            dependencies.embedding_gateway,
            dependencies.clock,
            dependencies.jitter,
            dependencies.http_client,
        )?;
        persistence.recover_interrupted_jobs()?;
        let chat_sink: Arc<dyn ChatEventSink> = events.clone();
        let chat = Arc::new(ChatService::with_http_client_and_runtime(
            persistence.database_path(),
            dependencies.credentials,
            chat_http_client,
            chat_sink,
            runtime.clone(),
        ));
        chat.recover_interrupted_turns()?;
        let worker_cancellation = CancellationToken::new();

        let core = Arc::new(Self {
            paths,
            runtime: runtime.clone(),
            _data_lock: data_lock,
            events,
            persistence,
            chat,
            workspace: Mutex::new(WorkspaceContext::default()),
            worker_cancellation,
            worker_task: Mutex::new(None),
        });

        let worker = core.persistence.clone();
        let cancellation = core.worker_cancellation.clone();
        let task = runtime.spawn(async move {
            worker.run_job_worker_until(cancellation).await;
        });
        *core
            .worker_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
        Ok(core)
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn runtime_handle(&self) -> &Handle {
        &self.runtime
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    pub fn persistence(&self) -> &PersistenceService {
        &self.persistence
    }

    pub fn chat(&self) -> Arc<ChatService> {
        Arc::clone(&self.chat)
    }

    pub fn open_workspace(
        &self,
        request: OpenWorkspaceRequest,
    ) -> Result<OpenWorkspaceResponse, WorkspaceCommandError> {
        let root_path = workspace_service::validate_workspace_root(&request.path)
            .map_err(WorkspaceCommandError::from)?;
        let kind = workspace_service::classify_workspace(&root_path)
            .map_err(WorkspaceCommandError::from)?;
        let greenfield = match kind {
            workspace_service::WorkspaceKind::Greenfield => Some(
                workspace_service::bootstrap_greenfield_workspace(&root_path)
                    .map_err(WorkspaceCommandError::from)?,
            ),
            workspace_service::WorkspaceKind::Brownfield => None,
        };
        let tree = workspace_service::build_workspace_tree(&root_path)
            .map_err(WorkspaceCommandError::from)?
            .tree;
        let sink: Arc<dyn WorkspaceEventSink> = self.events.clone();
        let watcher = WorkspaceWatcher::start(root_path.clone(), sink)
            .map_err(WorkspaceCommandError::from)?;

        let mut workspace = self.workspace.lock().map_err(|_| workspace_lock_error())?;
        workspace.root_path = Some(root_path.clone());
        workspace.watcher = Some(watcher);
        Ok(OpenWorkspaceResponse {
            root_path: root_path.to_string_lossy().to_string(),
            tree,
            kind,
            greenfield,
        })
    }

    pub fn bootstrap_greenfield_workspace(
        &self,
        request: workspace_service::BootstrapGreenfieldWorkspaceRequest,
    ) -> Result<BootstrapGreenfieldWorkspaceResponse, WorkspaceCommandError> {
        workspace_service::bootstrap_greenfield_workspace_by_path(request)
            .map_err(WorkspaceCommandError::from)
    }

    pub fn get_brownfield_onboarding(
        &self,
        project_id: &str,
    ) -> Result<BrownfieldOnboardingStatus, PersistenceCommandError> {
        self.persistence
            .get_brownfield_onboarding(project_id)
            .map_err(PersistenceCommandError::from)
    }

    pub fn set_brownfield_onboarding_consent(
        &self,
        request: SetBrownfieldConsentRequest,
    ) -> Result<BrownfieldOnboardingResult, WorkspaceCommandError> {
        let root = self.workspace_root()?;
        if workspace_service::classify_workspace(&root).map_err(WorkspaceCommandError::from)?
            != workspace_service::WorkspaceKind::Brownfield
        {
            return Err(WorkspaceCommandError {
                code: workspace_service::WorkspaceErrorCode::ValidationError,
                message: "O onboarding brownfield só pode ser alterado em um workspace existente."
                    .into(),
                details: None,
                remediation: None,
            });
        }
        let project = self
            .persistence
            .active_project_by_id(&request.project_id)
            .map_err(persistence_workspace_error)?;
        let project_root = fs::canonicalize(&project.workspace_path)
            .map_err(|error| WorkspaceCommandError::from(WorkspaceServiceError::Io(error)))?;
        if project_root != root {
            return Err(WorkspaceCommandError {
                code: workspace_service::WorkspaceErrorCode::ValidationError,
                message: "O projeto informado não corresponde ao workspace aberto.".into(),
                details: None,
                remediation: Some("Reabra o projeto correto e tente novamente.".into()),
            });
        }

        if !request.consent {
            let status = self
                .persistence
                .set_brownfield_onboarding_consent(&request.project_id, false)
                .map_err(persistence_workspace_error)?;
            return Ok(BrownfieldOnboardingResult {
                status,
                created_paths: Vec::new(),
                inventory: None,
            });
        }

        let created_paths = workspace_service::prepare_brownfield_knowledge_root(&root)
            .map_err(WorkspaceCommandError::from)?;
        let status = self
            .persistence
            .set_brownfield_onboarding_consent(&request.project_id, true)
            .map_err(persistence_workspace_error)?;
        let inventory = InventoryScanner::scan(
            &root,
            CollectionBudget::default(),
            &CancellationToken::new(),
        )
        .map_err(|error| WorkspaceCommandError {
            code: workspace_service::WorkspaceErrorCode::IoError,
            message: error.to_string(),
            details: None,
            remediation: Some("Tente atualizar o inventário novamente.".into()),
        })?;
        self.persistence
            .enqueue_brownfield_mapping(&request.project_id)
            .map_err(persistence_workspace_error)?;
        Ok(BrownfieldOnboardingResult {
            status,
            created_paths,
            inventory: Some(inventory),
        })
    }

    pub fn list_workspace_tree(&self) -> Result<WorkspaceTreeResponse, WorkspaceCommandError> {
        workspace_service::build_workspace_tree(&self.workspace_root()?)
            .map_err(WorkspaceCommandError::from)
    }

    pub fn read_workspace_file(
        &self,
        request: ReadWorkspaceFileRequest,
    ) -> Result<ReadWorkspaceFileResponse, WorkspaceCommandError> {
        workspace_service::read_workspace_file(&self.workspace_root()?, request)
            .map_err(WorkspaceCommandError::from)
    }

    pub fn write_workspace_file_atomic(
        &self,
        request: WriteWorkspaceFileAtomicRequest,
    ) -> Result<WriteWorkspaceFileAtomicResponse, WorkspaceCommandError> {
        workspace_service::write_workspace_file_atomic(&self.workspace_root()?, request)
            .map_err(WorkspaceCommandError::from)
    }

    pub fn search_workspace_text(
        &self,
        request: SearchWorkspaceTextRequest,
    ) -> Result<SearchWorkspaceTextResponse, WorkspaceCommandError> {
        workspace_service::search_workspace_text(&self.workspace_root()?, request)
            .map_err(WorkspaceCommandError::from)
    }

    pub fn get_workspace_vault_index(
        &self,
        request: GetWorkspaceVaultIndexRequest,
    ) -> Result<GetWorkspaceVaultIndexResponse, WorkspaceCommandError> {
        let root = self.workspace_root()?;
        self.persistence
            .document_cache
            .with(&root, |cache| {
                workspace_service::get_workspace_vault_index(&root, cache, request)
            })
            .map_err(WorkspaceCommandError::from)
    }

    pub fn get_context_candidates(
        &self,
        request: GetContextCandidatesRequest,
    ) -> Result<GetContextCandidatesResponse, WorkspaceCommandError> {
        let root = self.workspace_root()?;
        self.persistence
            .document_cache
            .with(&root, |cache| {
                workspace_service::get_context_candidates(&root, cache, request)
            })
            .map_err(WorkspaceCommandError::from)
    }

    pub fn rename_workspace_note(
        &self,
        request: RenameWorkspaceNoteRequest,
    ) -> Result<RenameWorkspaceNoteResponse, WorkspaceCommandError> {
        let root = self.workspace_root()?;
        self.persistence
            .document_cache
            .with(&root, |cache| {
                workspace_service::rename_workspace_note(&root, cache, request)
            })
            .map_err(WorkspaceCommandError::from)
    }

    pub fn workspace_root(&self) -> Result<PathBuf, WorkspaceCommandError> {
        self.workspace
            .lock()
            .map_err(|_| workspace_lock_error())?
            .root_path
            .clone()
            .ok_or_else(|| WorkspaceCommandError::from(WorkspaceServiceError::WorkspaceNotOpen))
    }

    pub fn is_shutting_down(&self) -> bool {
        self.worker_cancellation.is_cancelled()
    }

    pub async fn shutdown(&self) {
        // Primeiro bloqueia novos claims; em seguida, cancela jobs/LLM já registrados. O worker
        // continua vivo apenas o suficiente para persistir o estado terminal antes do join.
        self.worker_cancellation.cancel();
        self.persistence.cancel_all_active_jobs();
        self.chat.cancel_all_turns();
        if let Ok(mut workspace) = self.workspace.lock() {
            workspace.watcher = None;
        }

        let worker = self
            .worker_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut worker) = worker
            && tokio::time::timeout(Duration::from_secs(3), &mut worker)
                .await
                .is_err()
        {
            // A started spawn_blocking filesystem unit cannot be aborted. Keep the data lock
            // and join ownership until it has acknowledged completion; do not detach writes.
            eprintln!("supervisor shutdown exceeded 3s; awaiting the owned atomic unit");
            let _ = worker.await;
        }
        let _ = self.chat.wait_for_idle(Duration::from_secs(3)).await;
    }
}

fn workspace_lock_error() -> WorkspaceCommandError {
    WorkspaceCommandError {
        code: workspace_service::WorkspaceErrorCode::IoError,
        message: "Falha ao acessar o estado interno do workspace.".into(),
        details: None,
        remediation: None,
    }
}

fn persistence_workspace_error(error: PersistenceServiceError) -> WorkspaceCommandError {
    let code = match &error {
        PersistenceServiceError::Validation(_) => {
            workspace_service::WorkspaceErrorCode::ValidationError
        }
        PersistenceServiceError::NotFound(_) => workspace_service::WorkspaceErrorCode::NotFound,
        PersistenceServiceError::Cancelled(_) => workspace_service::WorkspaceErrorCode::Cancelled,
        _ => workspace_service::WorkspaceErrorCode::IoError,
    };
    WorkspaceCommandError {
        code,
        message: error.to_string(),
        details: None,
        remediation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_directory_lock_is_exclusive_and_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_data_dir(temp.path());
        let first = DataDirectoryLock::acquire(&paths).unwrap();
        assert!(matches!(
            DataDirectoryLock::acquire(&paths),
            Err(AppCoreError::DataDirectoryInUse)
        ));
        drop(first);
        DataDirectoryLock::acquire(&paths).unwrap();
    }

    #[test]
    fn app_paths_preserve_existing_database_name() {
        let paths = AppPaths::from_data_dir("C:/data/com.hardvibecode.desktop");
        assert!(paths.database_path().ends_with(DATABASE_NAME));
    }

    #[test]
    fn app_core_shutdown_cancels_and_joins_background_worker() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let core = AppCore::initialize(
            AppPaths::from_data_dir(temp.path()),
            runtime.handle().clone(),
        )
        .unwrap();

        runtime.block_on(core.shutdown());

        assert!(core.is_shutting_down());
        assert!(
            core.worker_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
        );
        drop(core);
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn brownfield_open_requires_scoped_consent_before_inventory_or_directories() {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("src")).unwrap();
        fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname='existing'\n",
        )
        .unwrap();
        fs::write(workspace.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let core = AppCore::initialize(
            AppPaths::from_data_dir(data.path()),
            runtime.handle().clone(),
        )
        .unwrap();

        let opened = core
            .open_workspace(OpenWorkspaceRequest {
                path: workspace.path().to_string_lossy().into_owned(),
            })
            .unwrap();
        assert_eq!(opened.kind, workspace_service::WorkspaceKind::Brownfield);
        assert!(opened.greenfield.is_none());
        assert!(!workspace.path().join("knowledge").exists());
        let project = core
            .persistence()
            .ensure_project_for_workspace(
                crate::persistence_service::EnsureProjectForWorkspaceRequest {
                    workspace_path: opened.root_path,
                    preferred_name: None,
                },
            )
            .unwrap()
            .project;
        let other_workspace = tempfile::tempdir().unwrap();
        fs::write(other_workspace.path().join("package.json"), "{}\n").unwrap();
        let other_project = core
            .persistence()
            .ensure_project_for_workspace(
                crate::persistence_service::EnsureProjectForWorkspaceRequest {
                    workspace_path: other_workspace.path().to_string_lossy().into_owned(),
                    preferred_name: None,
                },
            )
            .unwrap()
            .project;
        assert!(matches!(
            core.set_brownfield_onboarding_consent(SetBrownfieldConsentRequest {
                project_id: other_project.id,
                consent: true,
            }),
            Err(WorkspaceCommandError {
                code: workspace_service::WorkspaceErrorCode::ValidationError,
                ..
            })
        ));
        assert!(!other_workspace.path().join("knowledge").exists());
        assert_eq!(
            core.get_brownfield_onboarding(&project.id).unwrap().consent,
            crate::orchestration::brownfield::BrownfieldConsentState::NotDecided
        );

        let declined = core
            .set_brownfield_onboarding_consent(SetBrownfieldConsentRequest {
                project_id: project.id.clone(),
                consent: false,
            })
            .unwrap();
        assert_eq!(
            declined.status.consent,
            crate::orchestration::brownfield::BrownfieldConsentState::Declined
        );
        assert!(declined.inventory.is_none());
        assert!(!workspace.path().join("knowledge").exists());
        let jobs: i64 = rusqlite::Connection::open(core.persistence().database_path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE project_id = ?1",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(jobs, 0);

        let enabled = core
            .set_brownfield_onboarding_consent(SetBrownfieldConsentRequest {
                project_id: project.id.clone(),
                consent: true,
            })
            .unwrap();
        assert_eq!(
            enabled.status.consent,
            crate::orchestration::brownfield::BrownfieldConsentState::Granted
        );
        let inventory = enabled.inventory.expect("authorized inventory");
        assert_eq!(inventory.manifests, ["Cargo.toml"]);
        assert_eq!(inventory.source_files, 1);
        let components = workspace.path().join("knowledge/obsidian/components");
        assert!(components.is_dir());
        assert_eq!(fs::read_dir(components).unwrap().count(), 0);
        assert!(!workspace.path().join("docs/PROJECT_AGENT.md").exists());
        let mapping_jobs: i64 = rusqlite::Connection::open(core.persistence().database_path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE project_id=?1 AND kind='brownfield_map'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapping_jobs, 1);

        core.set_brownfield_onboarding_consent(SetBrownfieldConsentRequest {
            project_id: project.id,
            consent: false,
        })
        .unwrap();
        runtime.block_on(core.shutdown());
        drop(core);
        runtime.shutdown_timeout(Duration::from_secs(1));
    }
}
