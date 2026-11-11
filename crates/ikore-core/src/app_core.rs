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
use crate::embedding_service::{EmbeddingGateway, ModelPullEventSink, OllamaEmbeddingGateway};
use crate::events::{AppEvent, AppEventBus};
use crate::llm::{
    Clock, CredentialStore, JitterSource, KeyringCredentialStore, LlmEventSink, StableJitter,
    SystemClock,
};
use crate::orchestration::brownfield::{
    BrownfieldOnboardingResult, BrownfieldOnboardingStatus, InventoryScanner,
    SetBrownfieldConsentRequest, SetBrownfieldIgnoredRequest,
};
use crate::orchestration::{CollectionBudget, IndexingEventSink};
use crate::persistence_service::{
    PersistenceCommandError, PersistenceService, PersistenceServiceError,
};
use crate::terminals::{TerminalEventSink, TerminalService};
use crate::workspace_service::{
    self, BootstrapGreenfieldWorkspaceResponse, GetContextCandidatesRequest,
    GetContextCandidatesResponse, GetWorkspaceVaultIndexRequest, GetWorkspaceVaultIndexResponse,
    OpenWorkspaceRequest, OpenWorkspaceResponse, ReadWorkspaceFileRequest,
    ReadWorkspaceFileResponse, RenameWorkspaceNoteRequest, RenameWorkspaceNoteResponse,
    SearchWorkspaceTextRequest, SearchWorkspaceTextResponse, WorkspaceCommandError,
    WorkspaceEventSink, WorkspaceServiceError, WorkspaceTreeResponse, WorkspaceWatcher,
    WriteWorkspaceFileAtomicRequest, WriteWorkspaceFileAtomicResponse,
};

use crate::app_names::{
    APP_IDENTIFIER, DATABASE_FILE_NAME as DATABASE_NAME, LEGACY_APP_IDENTIFIER,
    LEGACY_LOCK_FILE_NAME, LOCK_FILE_NAME as LOCK_NAME, adopt_legacy_dir,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    data_dir: PathBuf,
    /// Large machine-local files that must not roam with the Windows profile.
    cache_dir: PathBuf,
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

        // A folder from before the rename to Ikore is renamed once (`app_names`).
        let base = PathBuf::from(base);
        let mut paths = Self::from_data_dir(adopt_legacy_dir(
            &base.join(LEGACY_APP_IDENTIFIER),
            &base.join(APP_IDENTIFIER),
        ));
        #[cfg(target_os = "windows")]
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            paths.cache_dir = adopt_legacy_dir(
                &local.join(LEGACY_APP_IDENTIFIER),
                &local.join(APP_IDENTIFIER),
            );
        }
        Ok(paths)
    }

    pub fn from_data_dir(path: impl Into<PathBuf>) -> Self {
        let data_dir = path.into();
        Self {
            cache_dir: data_dir.clone(),
            data_dir,
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The voice runtime and model (ADR-029): `%LOCALAPPDATA%` on Windows,
    /// so downloaded models and runtimes never travel with a roaming profile; the data directory
    /// when `HVC_DATA_DIR` isolates a run. Kept short for Windows' 260-character
    /// path limit.
    pub fn voice_dir(&self) -> PathBuf {
        self.cache_dir.join("voice")
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join(DATABASE_NAME)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppCoreError {
    #[error("could not resolve the application data directory")]
    DataDirectoryUnavailable,
    #[error("another Ikore instance is already using this data directory")]
    DataDirectoryInUse,
    #[error("I/O failure in the data directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to initialize persistence: {0}")]
    Persistence(#[from] PersistenceServiceError),
    #[error("failed to recover the Chat Companion: {0}")]
    Chat(#[from] crate::chat_service::ChatCommandError),
    #[error("failed to initialize the Agents panel: {0}")]
    Terminal(#[from] crate::terminals::TerminalError),
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

/// Holds exclusive access to an application data directory until dropped.
///
/// Keep the guard alive for as long as the directory is in use:
///
/// ```no_run
/// use ikore_core::{AppPaths, DataDirectoryLock};
/// # fn main() -> Result<(), ikore_core::app_core::AppCoreError> {
/// let paths = AppPaths::from_data_dir("target/example-data");
/// let _data_lock = DataDirectoryLock::acquire(&paths)?;
/// // Keep application services within the guard's scope.
/// # Ok(())
/// # }
/// ```
///
/// Discarding a successfully acquired guard releases the lock immediately:
///
/// ```compile_fail
/// #![deny(unused_must_use)]
/// use ikore_core::{AppPaths, DataDirectoryLock};
/// use ikore_core::app_core::AppCoreError;
///
/// fn initialize(paths: &AppPaths) -> Result<(), AppCoreError> {
///     DataDirectoryLock::acquire(paths)?;
///     Ok(())
/// }
/// ```
#[must_use = "Keep this guard alive while the application data directory is in use"]
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
            file.try_lock_exclusive().map_err(lock_error)?;
            // An instance built before the rename to Ikore holds the legacy lock
            // file. If it is still running, this folder is in use; otherwise its
            // stale lock file is removed.
            if let Some(directory) = lock_path.parent() {
                release_legacy_lock(&directory.join(LEGACY_LOCK_FILE_NAME))?;
            }
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

/// Maps a failed `try_lock_exclusive`. Another process holding the lock reports
/// `WouldBlock` on Unix but `ERROR_LOCK_VIOLATION` on Windows; both mean the
/// folder is in use.
fn lock_error(error: std::io::Error) -> AppCoreError {
    if error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
    {
        AppCoreError::DataDirectoryInUse
    } else {
        AppCoreError::Io(error)
    }
}

/// Fails with [`AppCoreError::DataDirectoryInUse`] while an instance built before
/// the rename to Ikore holds `legacy_lock`; removes the file when it is stale.
fn release_legacy_lock(legacy_lock: &Path) -> Result<(), AppCoreError> {
    if !legacy_lock.is_file() {
        return Ok(());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(legacy_lock)?;
    file.try_lock_exclusive().map_err(lock_error)?;
    let _ = FileExt::unlock(&file);
    drop(file);
    let _ = fs::remove_file(legacy_lock);
    Ok(())
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
    feature_agent: Arc<crate::feature_forge::FeatureAgentRuntime>,
    notes_scribe: Arc<crate::companion_notes::NoteScribeRuntime>,
    doc_reconciler: Arc<crate::doc_reconciliation::DocReconcilerRuntime>,
    voice: Arc<crate::voice::VoiceService>,
    terminals: Arc<TerminalService>,
    workspace: Mutex<WorkspaceContext>,
    worker_cancellation: CancellationToken,
    worker_task: Mutex<Option<JoinHandle<()>>>,
    _exchange_doorbell: ExchangeDoorbellTask,
}

/// How often the doorbell notifier looks for committed exchange log items.
pub const EXCHANGE_DOORBELL_INTERVAL: Duration = Duration::from_millis(250);

/// Stops the doorbell notifier when the core goes away (ADR-037 M2).
struct ExchangeDoorbellTask {
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl ExchangeDoorbellTask {
    fn start(database_path: PathBuf, events: Arc<AppEventBus>) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Err(error) = crate::exchange::log::spawn_notifier(
            database_path,
            events,
            stop.clone(),
            EXCHANGE_DOORBELL_INTERVAL,
        ) {
            // Without doorbells the UI keeps its polling cadence.
            eprintln!("exchange doorbell unavailable: {error}");
        }
        Self { stop }
    }

    fn stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for ExchangeDoorbellTask {
    fn drop(&mut self) {
        self.stop();
    }
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
        persistence.repair_state_version_drift()?;
        let chat_sink: Arc<dyn ChatEventSink> = events.clone();
        let chat = Arc::new(ChatService::with_http_client_and_runtime(
            persistence.database_path(),
            dependencies.credentials,
            chat_http_client,
            chat_sink,
            runtime.clone(),
        ));
        chat.recover_interrupted_turns()?;
        chat.attach_canvas_authoring(
            crate::companion_drawings::authoring_store::CanvasAuthoringStore::new(
                crate::companion_drawings::service::CompanionDrawingsService::new(
                    persistence.clone(),
                ),
            ),
        )?;
        let terminal_sink: Arc<dyn TerminalEventSink> = events.clone();
        let terminals = Arc::new(TerminalService::new(
            persistence.database_path(),
            runtime.clone(),
            terminal_sink,
        )?);
        chat.attach_terminal_service(Arc::clone(&terminals));
        persistence.attach_terminal_service(Arc::clone(&terminals));
        let indexing_sink: Arc<dyn IndexingEventSink> = events.clone();
        persistence.attach_indexing_event_sink(indexing_sink);
        let model_pull_sink: Arc<dyn ModelPullEventSink> = events.clone();
        persistence.attach_model_pull_event_sink(model_pull_sink);
        let feature_agent = Arc::new(crate::feature_forge::FeatureAgentRuntime::new(
            persistence.clone(),
            events.clone(),
            runtime.clone(),
        )?);
        let notes_scribe = Arc::new(crate::companion_notes::NoteScribeRuntime::new(
            persistence.clone(),
            events.clone(),
            runtime.clone(),
        )?);
        let doc_reconciler = Arc::new(crate::doc_reconciliation::DocReconcilerRuntime::new(
            persistence.clone(),
            events.clone(),
            runtime.clone(),
        ));
        let voice = crate::voice::VoiceService::new(
            ikore_voice::VoiceRuntime::new(paths.voice_dir()),
            persistence.clone(),
            events.clone(),
            runtime.clone(),
            notes_scribe.clone(),
        );
        let worker_cancellation = CancellationToken::new();
        let exchange_doorbell =
            ExchangeDoorbellTask::start(persistence.database_path().to_path_buf(), events.clone());

        let core = Arc::new(Self {
            paths,
            runtime: runtime.clone(),
            _data_lock: data_lock,
            events,
            persistence,
            chat,
            feature_agent,
            notes_scribe,
            doc_reconciler,
            voice,
            terminals,
            workspace: Mutex::new(WorkspaceContext::default()),
            worker_cancellation,
            worker_task: Mutex::new(None),
            _exchange_doorbell: exchange_doorbell,
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
        // Registers the push-to-talk shortcut when it is on and installed.
        core.voice.apply();
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

    pub fn drawings(&self) -> crate::companion_drawings::service::CompanionDrawingsService {
        crate::companion_drawings::service::CompanionDrawingsService::new(self.persistence.clone())
            .with_events(self.events.clone())
    }

    /// Detaches the active watcher before the filesystem rename. The UI checks
    /// unsaved editor buffers before calling this explicit user operation.
    pub fn delete_project_permanently(
        &self,
        project_id: &str,
        confirmation: &str,
    ) -> Result<crate::project_deletion::ProjectDeletionOutcome, PersistenceServiceError> {
        let _canvas_deletion_guard = self
            .chat
            .block_canvas_project_deletion(project_id)
            .map_err(|error| PersistenceServiceError::Runtime(error.message))?;
        self.drawings().with_project_guard(project_id, || {
            self.delete_project_with_drawings_idle(project_id, confirmation)
        })
    }

    fn delete_project_with_drawings_idle(
        &self,
        project_id: &str,
        confirmation: &str,
    ) -> Result<crate::project_deletion::ProjectDeletionOutcome, PersistenceServiceError> {
        let preview = self.persistence.preview_project_deletion(project_id)?;
        let target = fs::canonicalize(&preview.project.workspace_path)?;
        let active = {
            let mut workspace = self.workspace.lock().map_err(|_| {
                PersistenceServiceError::Runtime("Workspace lock is unavailable.".into())
            })?;
            let active = workspace
                .root_path
                .as_ref()
                .is_some_and(|root| root == &target);
            if active {
                workspace.watcher = None;
                workspace.root_path = None;
            }
            active
        };
        let result = self
            .persistence
            .delete_project_permanently(project_id, confirmation);
        if result.is_err() && active && target.exists() {
            self.open_workspace(OpenWorkspaceRequest {
                path: target.to_string_lossy().into_owned(),
            })
            .map_err(|error| {
                PersistenceServiceError::Runtime(format!(
                    "Project deletion failed, and the workspace watcher could not restart: {}",
                    error.message
                ))
            })?;
        }
        result
    }

    pub fn chat(&self) -> Arc<ChatService> {
        Arc::clone(&self.chat)
    }

    pub fn terminals(&self) -> Arc<TerminalService> {
        Arc::clone(&self.terminals)
    }
    pub fn feature_agent(&self) -> Arc<crate::feature_forge::FeatureAgentRuntime> {
        self.feature_agent.clone()
    }
    pub fn notes_scribe(&self) -> Arc<crate::companion_notes::NoteScribeRuntime> {
        self.notes_scribe.clone()
    }

    pub fn doc_reconciler(&self) -> Arc<crate::doc_reconciliation::DocReconcilerRuntime> {
        self.doc_reconciler.clone()
    }
    pub fn voice(&self) -> Arc<crate::voice::VoiceService> {
        self.voice.clone()
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
        let sink: Arc<dyn WorkspaceEventSink> =
            Arc::new(crate::events::MaintenanceWorkspaceEventSink::new(
                self.events.clone(),
                self.persistence.clone(),
                root_path.clone(),
            ));
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
                message: "Brownfield onboarding can only change in an existing workspace.".into(),
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
                message: "The project does not match the open workspace.".into(),
                details: None,
                remediation: Some("Reopen the right project and try again.".into()),
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
            remediation: Some("Try refreshing the inventory again.".into()),
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

    pub fn set_brownfield_path_ignored(
        &self,
        request: SetBrownfieldIgnoredRequest,
    ) -> Result<BrownfieldOnboardingStatus, WorkspaceCommandError> {
        let root = self.workspace_root()?;
        let project = self
            .persistence
            .active_project_by_id(&request.project_id)
            .map_err(persistence_workspace_error)?;
        let project_root = fs::canonicalize(&project.workspace_path)
            .map_err(|error| WorkspaceCommandError::from(WorkspaceServiceError::Io(error)))?;
        if project_root != root {
            return Err(WorkspaceCommandError {
                code: workspace_service::WorkspaceErrorCode::ValidationError,
                message: "The module does not belong to the open workspace.".into(),
                details: None,
                remediation: None,
            });
        }
        self.persistence
            .set_brownfield_path_ignored(
                &request.project_id,
                &request.relative_path,
                request.ignored,
            )
            .map_err(persistence_workspace_error)
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
        let from = request.from_relative_path.clone();
        let renamed = self
            .persistence
            .document_cache
            .with(&root, |cache| {
                workspace_service::rename_workspace_note(&root, cache, request)
            })
            .map_err(WorkspaceCommandError::from)?;
        // ADR-034 M5: the note keeps its reconciliation baselines and signals.
        if let Err(error) =
            self.persistence
                .rename_doc_drift_note(&root, &from, &renamed.renamed_to)
        {
            eprintln!("documentation reconciliation rows kept their old note path: {error}");
        }
        Ok(renamed)
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
        self._exchange_doorbell.stop();
        self.persistence.cancel_all_active_jobs();
        self.chat.cancel_all_turns();
        self.chat.cancel_all_canvas_jobs();
        self.feature_agent.cancel_all();
        self.notes_scribe.cancel_all();
        self.doc_reconciler.cancel_all();
        self.voice.shutdown();
        self.terminals.cancel_all();
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
        self.chat.wait_for_canvas_idle().await;
        self.feature_agent.wait_for_idle().await;
        self.notes_scribe.wait_for_idle().await;
        self.doc_reconciler.wait_for_idle().await;
        if !self.terminals.wait_for_idle(Duration::from_secs(3)).await {
            self.terminals.cancel_all();
            let _ = self.terminals.wait_for_idle(Duration::from_secs(30)).await;
        }
    }
}

fn workspace_lock_error() -> WorkspaceCommandError {
    WorkspaceCommandError {
        code: workspace_service::WorkspaceErrorCode::IoError,
        message: "Failed to access the internal workspace state.".into(),
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
        let _reacquired = DataDirectoryLock::acquire(&paths).unwrap();
    }

    #[test]
    fn app_paths_use_the_current_database_name() {
        let paths = AppPaths::from_data_dir("C:/data/com.ikore.desktop");
        assert!(paths.database_path().ends_with("ikore.db"));
        assert_eq!(DATABASE_NAME, "ikore.db");
    }

    #[test]
    fn a_running_legacy_instance_keeps_the_folder_in_use() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = temp.path().join(LEGACY_LOCK_FILE_NAME);
        let held = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&legacy)
            .unwrap();
        held.try_lock_exclusive().unwrap();

        let paths = AppPaths::from_data_dir(temp.path());
        assert!(matches!(
            DataDirectoryLock::acquire(&paths),
            Err(AppCoreError::DataDirectoryInUse)
        ));

        FileExt::unlock(&held).unwrap();
        drop(held);
        let _lock = DataDirectoryLock::acquire(&paths).unwrap();
        assert!(!legacy.exists(), "a stale legacy lock file is removed");
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
    fn project_deletion_preserves_canvas_until_acceptance_and_waits_for_running_jobs() {
        use crate::chat_service::canvas_authoring::{
            CanvasAuthoringRequestStateV1, CanvasAuthoringStore, CanvasOfferProposalV1,
        };
        use crate::companion_drawings::semantics::CanvasProfileV1;
        use rusqlite::params;

        for fault in [
            "confirmation",
            "path",
            "active",
            "terminal",
            "canvas",
            "accepted",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let workspace = directory.path().join("workspace");
            fs::create_dir(&workspace).unwrap();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let core = AppCore::initialize(
                AppPaths::from_data_dir(directory.path().join("data")),
                runtime.handle().clone(),
            )
            .unwrap();
            let project = core
                .persistence
                .ensure_project_for_workspace(
                    crate::persistence_service::EnsureProjectForWorkspaceRequest {
                        workspace_path: workspace.to_string_lossy().into_owned(),
                        preferred_name: Some("Canvas project".into()),
                    },
                )
                .unwrap()
                .project;
            let db = core.persistence.connection().unwrap();
            db.execute("INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at,transport_kind,cli_backend,model_source) VALUES('delete-chat',?1,'codex','default','Conversation','now','now','cli','codex','default')", [&project.id]).unwrap();
            db.execute("INSERT INTO chat_messages(id,conversation_id,ordinal,role,content,status,created_at,updated_at) VALUES('delete-source','delete-chat',0,'assistant','The UI calls the Core.','completed','now','now')", []).unwrap();
            let store = CanvasAuthoringStore::new(core.drawings());
            let mut offer = store
                .propose(
                    &project.id,
                    "delete-chat",
                    "delete-source",
                    &CanvasOfferProposalV1 {
                        profile: CanvasProfileV1::Architecture,
                        language: "en".into(),
                        title: "Architecture".into(),
                        request_text: "May I explain with Canvas?".into(),
                    },
                    1_000,
                )
                .unwrap();
            match fault {
                "path" => {
                    db.execute(
                        "UPDATE projects SET workspace_path='relative-untrusted-path' WHERE id=?1",
                        [&project.id],
                    )
                    .unwrap();
                }
                "active" => {
                    db.execute("INSERT INTO chat_messages(id,conversation_id,ordinal,role,content,status,created_at,updated_at) VALUES('active-message','delete-chat',1,'assistant','','streaming','now','now')", []).unwrap();
                }
                "terminal" => {
                    db.execute("INSERT INTO terminal_runs(id,project_id,agent_role,origin,execution_mode,backend,status,created_at,updated_at) VALUES('active-terminal',?1,'task_specifier','manual','cli_process','codex','running','now','now')", [&project.id]).unwrap();
                }
                "canvas" => {
                    let target = crate::companion_drawings::authoring::CanvasCompanionTargetSnapshot::capture_from_host(
                        crate::chat_service::ChatExecutionTargetRequest::Cli { backend: crate::terminals::CliBackend::Codex, model: None, model_profile: None }, "default",
                    ).unwrap();
                    offer = store
                        .admit(
                            &project.id,
                            "delete-chat",
                            &offer.request_id,
                            1,
                            "approve",
                            &target,
                            &serde_json::json!({"transport":"cli","backend":"codex"}),
                            1_000,
                            1_900,
                        )
                        .unwrap()
                        .request;
                }
                _ => {}
            }
            let confirmation = if fault == "confirmation" {
                "wrong name"
            } else {
                project.name.as_str()
            };
            let result = core.delete_project_permanently(&project.id, confirmation);
            if fault == "accepted" {
                assert!(result.unwrap().deleted);
                assert!(!workspace.exists());
                assert!(
                    store
                        .get(&project.id, "delete-chat", &offer.request_id)
                        .is_err()
                );
            } else {
                assert!(result.is_err(), "{fault}");
                let after = store
                    .get(&project.id, "delete-chat", &offer.request_id)
                    .unwrap();
                let expected = if fault == "canvas" {
                    CanvasAuthoringRequestStateV1::Running
                } else {
                    CanvasAuthoringRequestStateV1::AwaitingAuthorization
                };
                assert_eq!(after.state, expected, "{fault}");
                assert_eq!(after.request_revision, offer.request_revision, "{fault}");
                assert!(workspace.exists(), "{fault}");
                db.execute(
                    "UPDATE projects SET workspace_path=?1 WHERE id=?2",
                    params![workspace.to_string_lossy(), project.id],
                )
                .unwrap();
            }
            runtime.block_on(core.shutdown());
            drop(core);
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
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
