use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use hvc_core::AppCore;
use hvc_core::workspace_service::{
    FileNode, GetWorkspaceVaultIndexRequest, GetWorkspaceVaultIndexResponse,
    ReadWorkspaceFileRequest, RenameWorkspaceNoteRequest, RenameWorkspaceNoteResponse, SearchMode,
    SearchWorkspaceTextRequest, SearchWorkspaceTextResponse, VaultEdge, WorkspaceTreeResponse,
};

pub(crate) const VAULT_REFRESH_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(250);
pub(crate) const CONTEXT_DOCUMENTS_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(50);

pub(crate) type VaultFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'static>>;

pub(crate) trait VaultBackend: Send + Sync {
    fn tree(&self) -> VaultFuture<Vec<FileNode>>;
    fn index(&self, force_rebuild: bool) -> VaultFuture<GetWorkspaceVaultIndexResponse>;
    fn search(
        &self,
        request: SearchWorkspaceTextRequest,
    ) -> VaultFuture<SearchWorkspaceTextResponse>;
    fn read(&self, relative_path: String) -> VaultFuture<String>;
    fn rename(
        &self,
        request: RenameWorkspaceNoteRequest,
    ) -> VaultFuture<RenameWorkspaceNoteResponse>;
}

pub(crate) struct CoreVaultBackend {
    core: Arc<AppCore>,
}

impl CoreVaultBackend {
    pub(crate) fn new(core: Arc<AppCore>) -> Self {
        Self { core }
    }

    fn worker_error(operation: &str, error: tokio::task::JoinError) -> String {
        format!("worker de {operation} do Vault interrompido: {error}")
    }
}

impl VaultBackend for CoreVaultBackend {
    fn tree(&self) -> VaultFuture<Vec<FileNode>> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.list_workspace_tree()
                        .map(|WorkspaceTreeResponse { tree }| tree)
                        .map_err(|error| error.message)
                })
                .await
                .map_err(|error| Self::worker_error("árvore", error))?
        })
    }

    fn index(&self, force_rebuild: bool) -> VaultFuture<GetWorkspaceVaultIndexResponse> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.get_workspace_vault_index(GetWorkspaceVaultIndexRequest { force_rebuild })
                        .map_err(|error| error.message)
                })
                .await
                .map_err(|error| Self::worker_error("índice", error))?
        })
    }

    fn search(
        &self,
        request: SearchWorkspaceTextRequest,
    ) -> VaultFuture<SearchWorkspaceTextResponse> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.search_workspace_text(request)
                        .map_err(|error| error.message)
                })
                .await
                .map_err(|error| Self::worker_error("busca", error))?
        })
    }

    fn read(&self, relative_path: String) -> VaultFuture<String> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.read_workspace_file(ReadWorkspaceFileRequest { relative_path })
                        .map(|response| response.content)
                        .map_err(|error| error.message)
                })
                .await
                .map_err(|error| Self::worker_error("leitura", error))?
        })
    }

    fn rename(
        &self,
        request: RenameWorkspaceNoteRequest,
    ) -> VaultFuture<RenameWorkspaceNoteResponse> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.rename_workspace_note(request)
                        .map_err(|error| error.message)
                })
                .await
                .map_err(|error| Self::worker_error("renomeação", error))?
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VaultGraphNodeLayout {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VaultGraphNode {
    pub(crate) relative_path: String,
    pub(crate) title: String,
    pub(crate) layout: VaultGraphNodeLayout,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VaultGraphConnection {
    pub(crate) source: VaultGraphNodeLayout,
    pub(crate) target: VaultGraphNodeLayout,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct VaultGraphLayout {
    pub(crate) nodes: Vec<VaultGraphNode>,
    pub(crate) edges: Vec<VaultGraphConnection>,
}

pub struct VaultModel {
    pub workspace_root: Option<SharedString>,
    pub tree: Vec<FileNode>,
    pub index: Option<GetWorkspaceVaultIndexResponse>,
    pub documents: HashMap<String, String>,
    pub loading_documents: HashSet<String>,
    pub selected_path: Option<SharedString>,
    pub search_query: String,
    pub search_mode: SearchMode,
    pub search_case_sensitive: bool,
    pub search_response: Option<SearchWorkspaceTextResponse>,
    pub refreshing_tree: bool,
    pub indexing: bool,
    pub searching: bool,
    pub renaming: bool,
    pub stale: bool,
    pub graph_expanded: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
}

impl Default for VaultModel {
    fn default() -> Self {
        Self {
            workspace_root: None,
            tree: Vec::new(),
            index: None,
            documents: HashMap::new(),
            loading_documents: HashSet::new(),
            selected_path: None,
            search_query: String::new(),
            search_mode: SearchMode::Substring,
            search_case_sensitive: false,
            search_response: None,
            refreshing_tree: false,
            indexing: false,
            searching: false,
            renaming: false,
            stale: false,
            graph_expanded: false,
            error: None,
            notice: None,
        }
    }
}

impl VaultModel {
    pub(crate) fn hydrate_workspace(
        &mut self,
        workspace_root: String,
        tree: Vec<FileNode>,
        restored_selection: Option<String>,
    ) {
        *self = Self {
            workspace_root: Some(workspace_root.into()),
            tree,
            selected_path: restored_selection.map(Into::into),
            ..Self::default()
        };
    }

    pub(crate) fn apply_tree(&mut self, result: Result<Vec<FileNode>, String>) {
        self.refreshing_tree = false;
        match result {
            Ok(tree) => {
                self.tree = tree;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn apply_index(&mut self, result: Result<GetWorkspaceVaultIndexResponse, String>) {
        self.indexing = false;
        match result {
            Ok(index) => {
                let selected = self.selected_path.as_ref().map(AsRef::as_ref);
                let next = selected
                    .filter(|path| index.notes.iter().any(|note| note.relative_path == *path))
                    .map(str::to_owned)
                    .or_else(|| index.notes.first().map(|note| note.relative_path.clone()));
                self.selected_path = next.map(Into::into);
                self.index = Some(index);
                self.stale = false;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn select(&mut self, relative_path: Option<String>) {
        self.selected_path = relative_path.map(Into::into);
        self.notice = None;
    }

    pub(crate) fn search_request(&self) -> Option<SearchWorkspaceTextRequest> {
        let query = self.search_query.trim();
        (!query.is_empty() && self.workspace_root.is_some()).then(|| SearchWorkspaceTextRequest {
            query: query.to_owned(),
            mode: self.search_mode,
            case_sensitive: self.search_case_sensitive,
            max_results: 200,
        })
    }

    pub(crate) fn apply_search(&mut self, result: Result<SearchWorkspaceTextResponse, String>) {
        self.searching = false;
        match result {
            Ok(response) => {
                self.search_response = Some(response);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn begin_document_load(&mut self, relative_path: &str) -> bool {
        if self.documents.contains_key(relative_path)
            || self.loading_documents.contains(relative_path)
        {
            return false;
        }
        self.loading_documents.insert(relative_path.to_owned());
        true
    }

    pub(crate) fn finish_document_load(
        &mut self,
        relative_path: String,
        result: Result<String, String>,
    ) {
        self.loading_documents.remove(&relative_path);
        match result {
            Ok(content) => {
                self.documents.insert(relative_path, content);
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn apply_rename(
        &mut self,
        from_relative_path: &str,
        response: &RenameWorkspaceNoteResponse,
    ) {
        if let Some(content) = self.documents.remove(from_relative_path) {
            self.documents.insert(response.renamed_to.clone(), content);
        }
        if self.selected_path.as_ref().map(AsRef::as_ref) == Some(from_relative_path) {
            self.selected_path = Some(response.renamed_to.clone().into());
        }
        self.notice = Some(format!(
            "Renomeado para {} · {} referência(s) atualizada(s)",
            response.renamed_to, response.updated_references
        ));
        self.renaming = false;
        self.error = None;
    }

    pub(crate) fn selected_document(&self) -> Option<&str> {
        self.selected_path
            .as_ref()
            .and_then(|path| self.documents.get(path.as_ref()))
            .map(String::as_str)
    }

    pub(crate) fn context_paths(&self, active_tab_path: Option<&str>) -> Vec<String> {
        select_context_panel_paths(self.index.as_ref(), active_tab_path)
    }
}

pub(crate) fn normalize_rename_target(
    from_relative_path: &str,
    requested_name: &str,
) -> Result<Option<String>, String> {
    let requested_name = requested_name.trim();
    if requested_name.is_empty()
        || requested_name.contains('/')
        || requested_name.contains('\\')
        || requested_name.contains("..")
    {
        return Err("Informe um nome de arquivo sem diretórios ou segmentos de travessia.".into());
    }
    let file_name = if requested_name.to_ascii_lowercase().ends_with(".md") {
        requested_name.to_owned()
    } else {
        format!("{requested_name}.md")
    };
    let parent = from_relative_path
        .rsplit_once('/')
        .map(|(parent, _)| parent);
    let target = parent.map_or(file_name.clone(), |parent| format!("{parent}/{file_name}"));
    Ok((target != from_relative_path).then_some(target))
}

pub(crate) fn select_context_panel_paths(
    index: Option<&GetWorkspaceVaultIndexResponse>,
    active_tab_path: Option<&str>,
) -> Vec<String> {
    let mut paths = Vec::new();
    let mut add = |path: &str| {
        if !paths.iter().any(|candidate| candidate == path) {
            paths.push(path.to_owned());
        }
    };
    if let Some(path) = active_tab_path {
        add(path);
    }
    if let (Some(index), Some(path)) = (index, active_tab_path)
        && let Some(note) = index.notes.iter().find(|note| note.relative_path == path)
    {
        for outgoing in &note.outgoing_paths {
            add(outgoing);
        }
    }
    if let Some(index) = index {
        for note in &index.notes {
            add(&note.relative_path);
        }
    }
    paths.truncate(4);
    paths
}

pub(crate) fn build_vault_graph_layout(
    index: Option<&GetWorkspaceVaultIndexResponse>,
) -> VaultGraphLayout {
    let Some(index) = index.filter(|index| !index.notes.is_empty()) else {
        return VaultGraphLayout::default();
    };
    let width = 760.0;
    let height = 460.0;
    let columns = (index.notes.len() as f32).sqrt().ceil().max(1.0) as usize;
    let rows = index.notes.len().div_ceil(columns);
    let nodes = index
        .notes
        .iter()
        .enumerate()
        .map(|(position, note)| VaultGraphNode {
            relative_path: note.relative_path.clone(),
            title: if note.title.trim().is_empty() {
                note.relative_path
                    .strip_suffix(".md")
                    .unwrap_or(&note.relative_path)
                    .to_owned()
            } else {
                note.title.trim().to_owned()
            },
            layout: VaultGraphNodeLayout {
                x: ((position % columns) as f32 + 0.5) * (width / columns as f32),
                y: ((position / columns) as f32 + 0.5) * (height / rows as f32),
            },
        })
        .collect::<Vec<_>>();
    let nodes_by_path = nodes
        .iter()
        .map(|node| (node.relative_path.as_str(), node.layout))
        .collect::<HashMap<_, _>>();
    let edges = index
        .edges
        .iter()
        .filter_map(
            |VaultEdge {
                 source_path,
                 target_path,
             }| {
                Some(VaultGraphConnection {
                    source: *nodes_by_path.get(source_path.as_str())?,
                    target: *nodes_by_path.get(target_path.as_str())?,
                })
            },
        )
        .collect();
    VaultGraphLayout { nodes, edges }
}

#[cfg(test)]
mod tests {
    use hvc_core::workspace_service::{SearchMatch, VaultBrokenLink, VaultNoteSummary};

    use super::*;

    fn index() -> GetWorkspaceVaultIndexResponse {
        GetWorkspaceVaultIndexResponse {
            indexed_at: "2026-08-22T00:00:00Z".into(),
            notes: vec![
                VaultNoteSummary {
                    relative_path: "Features/Login.md".into(),
                    title: "Login".into(),
                    revision: "r1".into(),
                    frontmatter: None,
                    normalized_properties: None,
                    links: Vec::new(),
                    outgoing_paths: vec!["Architecture/Auth.md".into()],
                    backlink_paths: Vec::new(),
                    schema_warnings: Vec::new(),
                },
                VaultNoteSummary {
                    relative_path: "Architecture/Auth.md".into(),
                    title: "Auth".into(),
                    revision: "r2".into(),
                    frontmatter: None,
                    normalized_properties: None,
                    links: Vec::new(),
                    outgoing_paths: Vec::new(),
                    backlink_paths: vec!["Features/Login.md".into()],
                    schema_warnings: Vec::new(),
                },
            ],
            edges: vec![VaultEdge {
                source_path: "Features/Login.md".into(),
                target_path: "Architecture/Auth.md".into(),
            }],
            broken_links: Vec::<VaultBrokenLink>::new(),
        }
    }

    #[test]
    fn derives_deterministic_graph_and_context_layouts() {
        let index = index();
        assert_eq!(
            select_context_panel_paths(Some(&index), Some("Features/Login.md")),
            vec!["Features/Login.md", "Architecture/Auth.md"]
        );
        let first = build_vault_graph_layout(Some(&index));
        let second = build_vault_graph_layout(Some(&index));
        assert_eq!(first, second);
        assert_eq!(first.nodes.len(), 2);
        assert_eq!(first.edges.len(), 1);
        assert_eq!(first.nodes[0].relative_path, "Features/Login.md");
    }

    #[test]
    fn runs_typed_substring_and_regex_search() {
        let mut model = VaultModel {
            workspace_root: Some("C:\\vault".into()),
            search_query: " token ".into(),
            search_mode: SearchMode::Regex,
            search_case_sensitive: true,
            ..VaultModel::default()
        };
        let request = model.search_request().unwrap();
        assert_eq!(request.query, "token");
        assert!(matches!(request.mode, SearchMode::Regex));
        assert!(request.case_sensitive);
        assert_eq!(request.max_results, 200);
        model.apply_search(Ok(SearchWorkspaceTextResponse {
            matches: vec![SearchMatch {
                relative_path: "Features/Login.md".into(),
                line: 3,
                column: 4,
                line_text: "token".into(),
            }],
            truncated: false,
            scanned_files: 2,
        }));
        assert_eq!(model.search_response.unwrap().matches.len(), 1);
    }

    #[test]
    fn renames_note_and_refreshes_all_dependent_state() {
        let mut model = VaultModel {
            selected_path: Some("Features/Login.md".into()),
            documents: HashMap::from([("Features/Login.md".into(), "# Login".into())]),
            ..VaultModel::default()
        };
        let response = RenameWorkspaceNoteResponse {
            renamed_to: "Features/Sign-in.md".into(),
            updated_references: 1,
            affected_files: 1,
        };
        model.apply_rename("Features/Login.md", &response);
        let mut refreshed = index();
        refreshed.notes[0].relative_path = response.renamed_to.clone();
        refreshed.notes[0].title = "Sign-in".into();
        refreshed.edges[0].source_path = response.renamed_to.clone();
        model.apply_index(Ok(refreshed));

        assert_eq!(
            model.selected_path.as_ref().map(AsRef::as_ref),
            Some("Features/Sign-in.md")
        );
        assert_eq!(model.documents["Features/Sign-in.md"], "# Login");
        assert_eq!(model.index.as_ref().unwrap().edges.len(), 1);
    }

    #[test]
    fn deduplicates_overlapping_context_document_loads() {
        let mut model = VaultModel::default();
        assert!(model.begin_document_load("Architecture/Auth.md"));
        assert!(!model.begin_document_load("Architecture/Auth.md"));
        model.finish_document_load("Architecture/Auth.md".into(), Ok("# Auth".into()));
        assert_eq!(model.documents["Architecture/Auth.md"], "# Auth");
        assert!(!model.begin_document_load("Architecture/Auth.md"));
    }
}
