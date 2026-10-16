use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use hvc_core::AppCore;
use hvc_core::workspace_service::{
    FileNode, GetWorkspaceVaultIndexRequest, GetWorkspaceVaultIndexResponse,
    ReadWorkspaceFileRequest, RenameWorkspaceNoteRequest, RenameWorkspaceNoteResponse, SearchMode,
    SearchWorkspaceTextRequest, SearchWorkspaceTextResponse, VaultEdge, VaultNoteSummary,
    WorkspaceTreeResponse,
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
        format!("Vault {operation} worker interrupted: {error}")
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
                        .map(|WorkspaceTreeResponse { tree, .. }| tree)
                        .map_err(|error| error.message)
                })
                .await
                .map_err(|error| Self::worker_error("tree", error))?
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
                .map_err(|error| Self::worker_error("index", error))?
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
                .map_err(|error| Self::worker_error("rename", error))?
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
    /// Notes left out because the vault is larger than `MAX_GRAPH_NODES`.
    pub(crate) hidden: usize,
    /// Extent of the layout in world units; it grows with the node count and the camera fits it
    /// to the panel, so a larger graph is shown further out instead of being crammed.
    pub(crate) world_width: f32,
    pub(crate) world_height: f32,
}

/// Grid cell of the graph layout, in world units.
const GRAPH_CELL_WIDTH: f32 = 150.0;
const GRAPH_CELL_HEIGHT: f32 = 110.0;
/// Space kept around the relaxed layout so edge nodes and their labels are not flush.
const GRAPH_LAYOUT_MARGIN: f32 = 70.0;
/// Force-directed passes; with at most `MAX_GRAPH_NODES` nodes this is ~1.7M pair visits, run
/// once per index change (the layout is cached), never per frame.
const GRAPH_RELAX_PASSES: usize = 150;
pub(crate) const GRAPH_MIN_ZOOM: f32 = 0.1;
pub(crate) const GRAPH_MAX_ZOOM: f32 = 3.0;
/// The automatic fit never magnifies a small graph past this.
const GRAPH_FIT_MAX_ZOOM: f32 = 1.25;
const GRAPH_FIT_MARGIN: f32 = 48.0;

/// World-to-panel transform of the graph: `panel = world * zoom + pan`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GraphTransform {
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl GraphTransform {
    /// The whole layout centred in a `width` x `height` panel.
    pub(crate) fn fit(layout: &VaultGraphLayout, width: f32, height: f32) -> Self {
        let usable_w = (width - 2.0 * GRAPH_FIT_MARGIN).max(1.0);
        let usable_h = (height - 2.0 * GRAPH_FIT_MARGIN).max(1.0);
        let zoom = (usable_w / layout.world_width.max(1.0))
            .min(usable_h / layout.world_height.max(1.0))
            .clamp(GRAPH_MIN_ZOOM, GRAPH_FIT_MAX_ZOOM);
        Self {
            zoom,
            pan_x: (width - layout.world_width * zoom) / 2.0,
            pan_y: (height - layout.world_height * zoom) / 2.0,
        }
    }

    pub(crate) fn to_panel(self, x: f32, y: f32) -> (f32, f32) {
        (x * self.zoom + self.pan_x, y * self.zoom + self.pan_y)
    }

    /// Zoom by `factor` keeping the panel point (`anchor_x`, `anchor_y`) fixed under the cursor.
    pub(crate) fn zoomed(self, factor: f32, anchor_x: f32, anchor_y: f32) -> Self {
        let zoom = (self.zoom * factor).clamp(GRAPH_MIN_ZOOM, GRAPH_MAX_ZOOM);
        let ratio = zoom / self.zoom;
        Self {
            zoom,
            pan_x: anchor_x - (anchor_x - self.pan_x) * ratio,
            pan_y: anchor_y - (anchor_y - self.pan_y) * ratio,
        }
    }
}

/// Camera of the Vault graph. Until the developer pans or zooms, the view is recomputed as a fit
/// every frame (`adjusted == None`), so it follows window resizes and a growing vault.
#[derive(Debug, Default)]
pub(crate) struct GraphCamera {
    /// The developer's own transform, and the node count it was made for: a graph of a
    /// different size fits again.
    pub adjusted: Option<(GraphTransform, usize)>,
    /// Drag in progress: pointer and pan where it started, in panel coordinates.
    pub drag: Option<((f32, f32), (f32, f32))>,
    /// Panel bounds measured by the last paint; read by the next frame and by input handlers.
    pub bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    /// Transform the last frame was drawn with (the fit, unless adjusted) and its node count.
    pub last: std::rc::Rc<std::cell::Cell<Option<(GraphTransform, usize)>>>,
    /// Note whose dot the pointer rests on; its file name floats above the dot.
    pub hovered: Option<String>,
}

/// Fruchterman–Reingold relaxation of `points` inside a `width` x `height` area. Deterministic:
/// the same seed positions and links always give the same layout.
fn relax_layout(points: &mut [(f32, f32)], links: &[(usize, usize)], width: f32, height: f32) {
    let count = points.len();
    if count < 2 {
        return;
    }
    let ideal = ((width * height) / count as f32).sqrt() * 0.8;
    let (center_x, center_y) = (width / 2.0, height / 2.0);
    let mut temperature = width.max(height) / 8.0;
    let mut shift = vec![(0.0f32, 0.0f32); count];
    // Unlinked notes feel only the repulsion; a stronger pull keeps them in orbit around the
    // clusters instead of at the far edge, where they would force the fit to zoom far out.
    let mut linked = vec![false; count];
    for &(a, b) in links {
        linked[a] = true;
        linked[b] = true;
    }
    for _ in 0..GRAPH_RELAX_PASSES {
        shift.iter_mut().for_each(|value| *value = (0.0, 0.0));
        for a in 0..count {
            for b in a + 1..count {
                let dx = points[a].0 - points[b].0;
                let dy = points[a].1 - points[b].1;
                let distance = (dx * dx + dy * dy).sqrt().max(0.5);
                let push = ideal * ideal / distance;
                let (ux, uy) = (dx / distance, dy / distance);
                shift[a].0 += ux * push;
                shift[a].1 += uy * push;
                shift[b].0 -= ux * push;
                shift[b].1 -= uy * push;
            }
        }
        for &(a, b) in links {
            let dx = points[a].0 - points[b].0;
            let dy = points[a].1 - points[b].1;
            let distance = (dx * dx + dy * dy).sqrt().max(0.5);
            let pull = distance * distance / ideal;
            let (ux, uy) = (dx / distance, dy / distance);
            shift[a].0 -= ux * pull;
            shift[a].1 -= uy * pull;
            shift[b].0 += ux * pull;
            shift[b].1 += uy * pull;
        }
        for ((point, (sx, sy)), linked) in points.iter_mut().zip(&shift).zip(&linked) {
            let gravity = if *linked { 0.05 } else { 0.25 } * ideal / 10.0;
            let gx = sx + (center_x - point.0) * gravity;
            let gy = sy + (center_y - point.1) * gravity;
            let length = (gx * gx + gy * gy).sqrt().max(0.001);
            let step = length.min(temperature);
            point.0 += gx / length * step;
            point.1 += gy / length * step;
        }
        temperature *= 0.96;
    }
}

/// Radius of a node dot on screen: it scales with the zoom within readable bounds.
pub(crate) fn graph_node_radius(zoom: f32) -> f32 {
    8.0 * zoom.clamp(0.7, 1.3)
}

impl GraphCamera {
    pub(crate) fn transform(
        &self,
        layout: &VaultGraphLayout,
        width: f32,
        height: f32,
    ) -> GraphTransform {
        match self.adjusted {
            Some((transform, nodes)) if nodes == layout.nodes.len() => transform,
            _ => GraphTransform::fit(layout, width, height),
        }
    }

    /// What one frame needs, detached from the model borrow (two `Rc`s and plain values).
    pub(crate) fn snapshot(&self) -> GraphCamera {
        GraphCamera {
            adjusted: self.adjusted,
            drag: self.drag,
            bounds: self.bounds.clone(),
            last: self.last.clone(),
            hovered: self.hovered.clone(),
        }
    }
}

/// Nodes the graph draws. Past this the fixed 760×460 canvas is unreadable anyway and every
/// frame would lay out thousands of elements; the most connected notes (and the selected one)
/// are kept.
pub(crate) const MAX_GRAPH_NODES: usize = 150;
/// Note bodies kept for the inspector and context panels; older ones reload on demand.
const MAX_CACHED_DOCUMENTS: usize = 64;

pub struct VaultModel {
    pub workspace_root: Option<SharedString>,
    pub tree: Vec<FileNode>,
    pub(crate) folders: crate::file_tree::FolderState,
    pub(crate) camera: GraphCamera,
    /// The relaxed layout and what it was built from; rebuilt only when the index (or, for a
    /// capped vault, the selection) changes.
    graph_cache: std::cell::RefCell<Option<(GraphCacheKey, VaultGraphLayout)>>,
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
            folders: crate::file_tree::FolderState::default(),
            camera: GraphCamera::default(),
            graph_cache: Default::default(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphCacheKey {
    indexed_at: String,
    notes: usize,
    edges: usize,
    /// Only a capped vault's node set depends on the selection.
    selected: Option<String>,
}

impl VaultModel {
    /// The graph layout for the current index, from the cache when nothing it depends on changed.
    pub(crate) fn graph_layout(&self, selected: Option<&str>) -> VaultGraphLayout {
        let key = self.index.as_ref().map(|index| GraphCacheKey {
            indexed_at: index.indexed_at.clone(),
            notes: index.notes.len(),
            edges: index.edges.len(),
            selected: (index.notes.len() > MAX_GRAPH_NODES)
                .then(|| selected.map(str::to_owned))
                .flatten(),
        });
        let Some(key) = key else {
            return VaultGraphLayout::default();
        };
        if let Some((cached_key, layout)) = self.graph_cache.borrow().as_ref()
            && *cached_key == key
        {
            return layout.clone();
        }
        let layout = build_vault_graph_layout(self.index.as_ref(), selected);
        *self.graph_cache.borrow_mut() = Some((key, layout.clone()));
        layout
    }

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
                if self.documents.len() >= MAX_CACHED_DOCUMENTS {
                    let selected = self.selected_path.clone();
                    let evict = self
                        .documents
                        .keys()
                        .find(|path| selected.as_ref().map(AsRef::as_ref) != Some(path.as_str()))
                        .cloned();
                    if let Some(evict) = evict {
                        self.documents.remove(&evict);
                    }
                }
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
            "Renamed to {} · {} reference(s) updated",
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
        return Err("Enter a file name without directories or path traversal segments.".into());
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
    selected: Option<&str>,
) -> VaultGraphLayout {
    let Some(index) = index.filter(|index| !index.notes.is_empty()) else {
        return VaultGraphLayout::default();
    };
    let shown: Vec<&VaultNoteSummary> = if index.notes.len() <= MAX_GRAPH_NODES {
        index.notes.iter().collect()
    } else {
        let mut degree: HashMap<&str, usize> = HashMap::new();
        for edge in &index.edges {
            *degree.entry(edge.source_path.as_str()).or_default() += 1;
            *degree.entry(edge.target_path.as_str()).or_default() += 1;
        }
        let mut ranked: Vec<usize> = (0..index.notes.len()).collect();
        ranked.sort_by_key(|position| {
            let path = index.notes[*position].relative_path.as_str();
            (
                Some(path) != selected,
                std::cmp::Reverse(degree.get(path).copied().unwrap_or(0)),
                *position,
            )
        });
        ranked.truncate(MAX_GRAPH_NODES);
        // Back to index order so the grid is stable as notes gain or lose links.
        ranked.sort_unstable();
        ranked
            .into_iter()
            .map(|position| &index.notes[position])
            .collect()
    };
    let hidden = index.notes.len() - shown.len();
    // A landscape grid whose extent grows with the node count; the camera fits it to the panel.
    let columns = ((shown.len() as f32 * 1.6).sqrt().ceil() as usize).clamp(1, shown.len().max(1));
    let rows = shown.len().div_ceil(columns);
    let world_width = columns as f32 * GRAPH_CELL_WIDTH;
    let world_height = rows.max(1) as f32 * GRAPH_CELL_HEIGHT;
    let nodes = shown
        .into_iter()
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
                x: ((position % columns) as f32 + 0.5) * GRAPH_CELL_WIDTH,
                y: ((position / columns) as f32 + 0.5) * GRAPH_CELL_HEIGHT,
            },
        })
        .collect::<Vec<_>>();
    // Grid positions seed a force-directed relaxation: linked notes pull together, every pair
    // pushes apart, so clusters read as clusters instead of a mesh of crossing lines.
    let mut nodes = nodes;
    {
        let slot: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.relative_path.as_str(), position))
            .collect();
        let links: Vec<(usize, usize)> = index
            .edges
            .iter()
            .filter_map(|edge| {
                Some((
                    *slot.get(edge.source_path.as_str())?,
                    *slot.get(edge.target_path.as_str())?,
                ))
            })
            .filter(|(a, b)| a != b)
            .collect();
        let mut points: Vec<(f32, f32)> = nodes
            .iter()
            .map(|node| (node.layout.x, node.layout.y))
            .collect();
        relax_layout(&mut points, &links, world_width, world_height);
        let min_x = points.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let min_y = points.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        for (node, (x, y)) in nodes.iter_mut().zip(points) {
            node.layout = VaultGraphNodeLayout {
                x: x - min_x + GRAPH_LAYOUT_MARGIN,
                y: y - min_y + GRAPH_LAYOUT_MARGIN,
            };
        }
    }
    let world_width = nodes.iter().map(|n| n.layout.x).fold(0.0, f32::max) + GRAPH_LAYOUT_MARGIN;
    let world_height = nodes.iter().map(|n| n.layout.y).fold(0.0, f32::max) + GRAPH_LAYOUT_MARGIN;
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
    VaultGraphLayout {
        nodes,
        edges,
        hidden,
        world_width,
        world_height,
    }
}

#[cfg(test)]
mod tests {
    use hvc_core::workspace_service::{SearchMatch, VaultBrokenLink};

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
                    source_files: Vec::new(),
                    test_files: Vec::new(),
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
                    source_files: Vec::new(),
                    test_files: Vec::new(),
                },
            ],
            edges: vec![VaultEdge {
                source_path: "Features/Login.md".into(),
                target_path: "Architecture/Auth.md".into(),
            }],
            broken_links: Vec::<VaultBrokenLink>::new(),
            broken_file_references: Vec::new(),
        }
    }

    #[test]
    fn a_large_vault_draws_its_most_connected_notes_and_keeps_the_selected_one() {
        let mut large = index();
        let template = large.notes[0].clone();
        large.notes = (0..MAX_GRAPH_NODES + 50)
            .map(|position| VaultNoteSummary {
                relative_path: format!("n{position}.md"),
                title: format!("n{position}"),
                ..template.clone()
            })
            .collect();
        // n199 is the hub: linked from 10 notes. n5 is linked once.
        large.edges = (0..10)
            .map(|source| VaultEdge {
                source_path: format!("n{source}.md"),
                target_path: format!("n{}.md", MAX_GRAPH_NODES + 49),
            })
            .collect();
        let last = format!("n{}.md", MAX_GRAPH_NODES + 48);
        let graph = build_vault_graph_layout(Some(&large), Some(last.as_str()));
        assert_eq!(graph.nodes.len(), MAX_GRAPH_NODES);
        assert_eq!(graph.hidden, 50);
        let shown: Vec<&str> = graph
            .nodes
            .iter()
            .map(|n| n.relative_path.as_str())
            .collect();
        assert!(
            shown.contains(&format!("n{}.md", MAX_GRAPH_NODES + 49).as_str()),
            "hub kept"
        );
        assert!(shown.contains(&last.as_str()), "selection kept");
        assert_eq!(graph.edges.len(), 10);
        // Index order is preserved for a stable grid.
        assert!(shown.windows(2).all(|pair| {
            let key = |p: &str| p[1..p.len() - 3].parse::<usize>().unwrap();
            key(pair[0]) < key(pair[1])
        }));
    }

    #[test]
    fn a_growing_graph_is_fitted_further_out_and_zoom_keeps_the_cursor_point() {
        let mut small = index();
        let template = small.notes[0].clone();
        let layout_for = |count: usize, index: &mut GetWorkspaceVaultIndexResponse| {
            index.notes = (0..count)
                .map(|position| VaultNoteSummary {
                    relative_path: format!("n{position}.md"),
                    ..template.clone()
                })
                .collect();
            index.edges.clear();
            build_vault_graph_layout(Some(index), None)
        };
        let few = layout_for(4, &mut small);
        let many = layout_for(120, &mut small);
        let fit_few = GraphTransform::fit(&few, 600.0, 500.0);
        let fit_many = GraphTransform::fit(&many, 600.0, 500.0);
        assert!(fit_many.zoom < fit_few.zoom, "{fit_many:?} vs {fit_few:?}");
        assert!(fit_few.zoom <= GRAPH_FIT_MAX_ZOOM);
        // The fitted graph sits inside the panel.
        let (left, top) = fit_many.to_panel(0.0, 0.0);
        let (right, bottom) = fit_many.to_panel(many.world_width, many.world_height);
        assert!(left >= 0.0 && top >= 0.0 && right <= 600.0 && bottom <= 500.0);

        // Zooming around a point keeps that point under the cursor.
        let world = (200.0, 150.0);
        let before = fit_many.to_panel(world.0, world.1);
        let zoomed = fit_many.zoomed(1.5, before.0, before.1);
        let after = zoomed.to_panel(world.0, world.1);
        assert!((after.0 - before.0).abs() < 0.01 && (after.1 - before.1).abs() < 0.01);
        assert!(fit_many.zoomed(1000.0, 0.0, 0.0).zoom <= GRAPH_MAX_ZOOM);
    }

    #[test]
    fn the_note_cache_is_bounded_and_keeps_the_selected_note() {
        let mut model = VaultModel {
            selected_path: Some("keep.md".into()),
            ..VaultModel::default()
        };
        model.finish_document_load("keep.md".into(), Ok("selected".into()));
        for position in 0..MAX_CACHED_DOCUMENTS + 20 {
            model.finish_document_load(format!("n{position}.md"), Ok("body".into()));
        }
        assert_eq!(model.documents.len(), MAX_CACHED_DOCUMENTS);
        assert!(model.documents.contains_key("keep.md"));
    }

    #[test]
    fn derives_deterministic_graph_and_context_layouts() {
        let index = index();
        assert_eq!(
            select_context_panel_paths(Some(&index), Some("Features/Login.md")),
            vec!["Features/Login.md", "Architecture/Auth.md"]
        );
        let first = build_vault_graph_layout(Some(&index), None);
        let second = build_vault_graph_layout(Some(&index), None);
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
