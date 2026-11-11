use std::path::PathBuf;

use ikore_core::workspace_service::{
    GetWorkspaceVaultIndexRequest, VaultIndexCache, build_workspace_tree, get_workspace_vault_index,
};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_vault")
}

#[test]
fn sample_vault_builds_tree_and_resolves_wikilinks() {
    let root = fixture_root();
    let tree = build_workspace_tree(&root).expect("fixture tree should build");
    assert_eq!(tree.tree.len(), 2);

    let mut cache = VaultIndexCache::default();
    let index = get_workspace_vault_index(
        &root,
        &mut cache,
        GetWorkspaceVaultIndexRequest {
            force_rebuild: true,
        },
    )
    .expect("fixture vault should index");

    assert_eq!(index.notes.len(), 2);
    assert_eq!(index.edges.len(), 1);
    assert!(index.broken_links.is_empty());
    assert_eq!(index.edges[0].source_path, "feature-login.md");
    assert_eq!(index.edges[0].target_path, "architecture/auth.md");
}
