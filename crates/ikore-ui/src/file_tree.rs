//! Flattened, collapsible rows for the Workspace and Vault explorers.
//!
//! Rows carry only what a row paints (depth, name, path, kind, open state) — never the
//! `FileNode` with its children — so flattening costs O(visible rows) instead of copying every
//! subtree once per ancestor on every frame.

use std::collections::HashSet;

use gpui::SharedString;
use ikore_core::workspace_service::{FileNode, FileNodeKind, MAX_TREE_FILES};

/// Up to this many files every folder starts open; above it folders start closed so a large
/// project renders its top level, not thousands of rows.
pub(crate) const EXPAND_ALL_MAX_FILES: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeRow {
    pub depth: usize,
    pub name: SharedString,
    pub relative_path: String,
    pub is_dir: bool,
    /// Meaningful for folders only.
    pub expanded: bool,
}

/// Folder open/closed state: the default depends on the tree size, and `toggled` records the
/// folders the developer flipped away from that default.
#[derive(Debug, Default, Clone)]
pub(crate) struct FolderState {
    pub toggled: HashSet<String>,
}

impl FolderState {
    pub(crate) fn toggle(&mut self, relative_path: &str) {
        if !self.toggled.remove(relative_path) {
            self.toggled.insert(relative_path.to_owned());
        }
    }

    fn expanded(&self, relative_path: &str, default_open: bool) -> bool {
        default_open != self.toggled.contains(relative_path)
    }
}

pub(crate) fn count_files(nodes: &[FileNode]) -> usize {
    nodes
        .iter()
        .map(|node| match node.kind {
            FileNodeKind::File => 1,
            FileNodeKind::Directory => count_files(&node.children),
        })
        .sum()
}

/// Whether the core stopped listing at `MAX_TREE_FILES`.
pub(crate) fn is_capped(nodes: &[FileNode]) -> bool {
    count_files(nodes) >= MAX_TREE_FILES
}

/// Visible rows: children of a closed folder are skipped entirely.
pub(crate) fn flatten(nodes: &[FileNode], folders: &FolderState) -> Vec<TreeRow> {
    let default_open = count_files(nodes) <= EXPAND_ALL_MAX_FILES;
    let mut rows = Vec::new();
    push_rows(nodes, 0, folders, default_open, &mut rows);
    rows
}

fn push_rows(
    nodes: &[FileNode],
    depth: usize,
    folders: &FolderState,
    default_open: bool,
    rows: &mut Vec<TreeRow>,
) {
    for node in nodes {
        let is_dir = matches!(node.kind, FileNodeKind::Directory);
        let expanded = is_dir && folders.expanded(&node.relative_path, default_open);
        rows.push(TreeRow {
            depth,
            name: node.name.clone().into(),
            relative_path: node.relative_path.clone(),
            is_dir,
            expanded,
        });
        if expanded {
            push_rows(&node.children, depth + 1, folders, default_open, rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> FileNode {
        FileNode {
            name: path.rsplit('/').next().unwrap().into(),
            relative_path: path.into(),
            kind: FileNodeKind::File,
            children: Vec::new(),
        }
    }

    fn dir(path: &str, children: Vec<FileNode>) -> FileNode {
        FileNode {
            name: path.rsplit('/').next().unwrap().into(),
            relative_path: path.into(),
            kind: FileNodeKind::Directory,
            children,
        }
    }

    #[test]
    fn small_trees_start_open_and_a_toggle_closes_one_folder() {
        let tree = vec![
            dir(
                "docs",
                vec![
                    file("docs/a.md"),
                    dir("docs/deep", vec![file("docs/deep/b.md")]),
                ],
            ),
            file("README.md"),
        ];
        let mut folders = FolderState::default();
        let names: Vec<_> = flatten(&tree, &folders)
            .into_iter()
            .map(|row| (row.depth, row.relative_path))
            .collect();
        assert_eq!(
            names,
            vec![
                (0, "docs".to_string()),
                (1, "docs/a.md".to_string()),
                (1, "docs/deep".to_string()),
                (2, "docs/deep/b.md".to_string()),
                (0, "README.md".to_string()),
            ]
        );

        folders.toggle("docs");
        let rows = flatten(&tree, &folders);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].is_dir && !rows[0].expanded);

        folders.toggle("docs");
        assert_eq!(flatten(&tree, &folders).len(), 5);
    }

    #[test]
    fn large_trees_start_closed_and_open_one_folder_at_a_time() {
        let files: Vec<_> = (0..=EXPAND_ALL_MAX_FILES)
            .map(|index| file(&format!("big/{index}.md")))
            .collect();
        let tree = vec![dir("big", files), file("top.md")];
        let mut folders = FolderState::default();
        let rows = flatten(&tree, &folders);
        assert_eq!(rows.len(), 2, "only the top level renders");
        folders.toggle("big");
        assert_eq!(flatten(&tree, &folders).len(), EXPAND_ALL_MAX_FILES + 3);
        assert!(!is_capped(&tree));
    }
}
