use super::*;
use crate::feature_forge::{FeatureBrief, FeatureEvaluation, FeatureState, TaskDraft, TaskKind};

fn task(title: &str) -> WorkTask {
    WorkTask {
        id: "task-1".into(),
        project_id: "project-1".into(),
        revision: 1,
        draft: TaskDraft {
            title: title.into(),
            kind: TaskKind::Feature,
            description: title.into(),
            criteria: vec![],
            dependencies: vec![],
            feature_id: None,
        },
        feature_revision: None,
        state: "ready".into(),
        active_pulsation_id: None,
        completed_pulsation_id: None,
    }
}

fn feature(scope: &str) -> Feature {
    Feature {
        id: "feature-1".into(),
        project_id: "project-1".into(),
        title: "Notes".into(),
        original_idea: scope.into(),
        revision: 2,
        approved_revision: Some(2),
        state: FeatureState::Planned,
        brief: FeatureBrief {
            scope: scope.into(),
            ..Default::default()
        },
        evaluation: FeatureEvaluation::default(),
        structural_level: 0,
        wave: None,
        position: 0,
        archive_reason: None,
    }
}

fn paths(result: &DestinationResolution) -> Vec<&str> {
    result.task_destinations[0]
        .paths
        .iter()
        .map(RelPath::as_str)
        .collect()
}

fn write(root: &Path, path: &str, content: &str) {
    let target = root.join(path);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(target, content).unwrap();
}

#[test]
fn greenfield_static_scope_receives_application_destinations_without_changing_approved_scope() {
    let root = tempfile::tempdir().unwrap();
    let approved = feature("A static responsive web application using HTML, CSS and JavaScript.");
    let mut selected = task("Implement the note editor");
    selected.draft.feature_id = Some(approved.id.clone());
    let before = (selected.clone(), approved.clone());
    let resolved = resolve_task_destinations(
        root.path(),
        std::slice::from_ref(&selected),
        std::slice::from_ref(&approved),
        &HashMap::new(),
    )
    .unwrap();
    assert!(resolved.greenfield);
    assert!(paths(&resolved).contains(&"index.html"));
    assert!(paths(&resolved).iter().any(|path| path.ends_with(".js")));
    assert_eq!((selected, approved), before);
    assert_eq!(
        resolved.task_destinations[0].origin,
        DestinationOrigin::RepositoryConvention
    );
    assert!(!root.path().join("index.html").exists());
}

#[test]
fn product_planning_uses_a_document_instead_of_inventing_an_application_stack() {
    let root = tempfile::tempdir().unwrap();
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Define NoteAPP product vision and MVP boundaries")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(paths(&resolved), vec!["docs/product-vision.md"]);
}

#[test]
fn unknown_stack_records_an_architecture_destination_and_a_visible_notice() {
    let root = tempfile::tempdir().unwrap();
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Implement the calendar")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(paths(&resolved), vec!["docs/architecture.md"]);
    assert!(
        resolved
            .notices
            .iter()
            .any(|notice| notice.contains("architecture"))
    );
}

#[test]
fn next_manifest_and_src_app_convention_are_respected_without_code_evidence() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "package.json",
        r#"{"dependencies":{"next":"15","react":"19"}}"#,
    );
    write(
        root.path(),
        "src/app/layout.tsx",
        "export default function Layout() {}\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Add calendar page")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(
        paths(&resolved)
            .iter()
            .any(|path| path.starts_with("src/app/"))
    );
    assert!(
        paths(&resolved)
            .iter()
            .all(|path| !path.starts_with("app/"))
    );
    assert!(!resolved.greenfield);
}

#[test]
fn next_root_app_convention_does_not_create_a_parallel_src_tree() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "package.json",
        r#"{"dependencies":{"next":"15"}}"#,
    );
    write(
        root.path(),
        "app/layout.js",
        "export default function Layout() {}\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Add calendar page")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(paths(&resolved).iter().any(|path| path.starts_with("app/")));
    assert!(
        paths(&resolved)
            .iter()
            .all(|path| !path.starts_with("src/"))
    );
}

#[test]
fn existing_relevant_module_and_tests_are_preferred() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "package.json",
        r#"{"devDependencies":{"vite":"6","vitest":"3"},"type":"module"}"#,
    );
    write(
        root.path(),
        "src/calendar.ts",
        "export const calendar = {};\n",
    );
    write(
        root.path(),
        "tests/calendar.test.ts",
        "test('calendar', () => {});\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Fix calendar boundaries")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(paths(&resolved).contains(&"src/calendar.ts"));
    assert!(paths(&resolved).contains(&"tests/calendar.test.ts"));
    assert_eq!(paths(&resolved).len(), 2);
}

#[test]
fn relevant_source_without_a_test_receives_a_planned_test_destination() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "src/calendar.ts",
        "export const calendar = {};\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Fix calendar boundaries")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(
        paths(&resolved),
        vec!["src/calendar.ts", "tests/calendar.test.ts"]
    );
}

#[test]
fn description_finds_a_relevant_module_under_a_generic_task_title() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "src/calendar.ts",
        "export const calendar = {};\n",
    );
    write(root.path(), "src/editor.ts", "export const editor = {};\n");
    let mut selected = task("Improve validation");
    selected.draft.description = "Reject impossible dates in the calendar module.".into();
    let resolved =
        resolve_task_destinations(root.path(), &[selected], &[], &HashMap::new()).unwrap();
    assert!(paths(&resolved).contains(&"src/calendar.ts"));
    assert!(paths(&resolved).contains(&"tests/calendar.test.ts"));
    assert!(!paths(&resolved).contains(&"src/editor.ts"));
}

#[test]
fn planned_test_preserves_an_observed_adjacent_test_convention() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "src/calendar.ts",
        "export const calendar = {};\n",
    );
    write(
        root.path(),
        "src/editor.test.ts",
        "test('editor', () => {});\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Fix calendar boundaries")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(paths(&resolved).contains(&"src/calendar.test.ts"));
    assert!(!paths(&resolved).contains(&"tests/calendar.test.ts"));
}

#[test]
fn planned_test_preserves_an_observed_adjacent_jsx_test_convention() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "src/calendar.jsx",
        "export const Calendar = () => null;\n",
    );
    write(
        root.path(),
        "src/editor.test.jsx",
        "test('editor', () => {});\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Fix calendar boundaries")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(paths(&resolved).contains(&"src/calendar.test.jsx"));
    assert!(!paths(&resolved).contains(&"tests/calendar.test.jsx"));
}

#[test]
fn planned_test_preserves_an_observed_dunder_tests_convention() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "src/calendar.ts",
        "export const calendar = {};\n",
    );
    write(
        root.path(),
        "src/__tests__/editor.test.ts",
        "test('editor', () => {});\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Fix calendar boundaries")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(paths(&resolved).contains(&"src/__tests__/calendar.test.ts"));
    assert!(!paths(&resolved).contains(&"tests/calendar.test.ts"));
}

#[test]
fn title_matches_have_priority_over_description_matches() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "src/calendar.ts",
        "export const calendar = {};\n",
    );
    write(root.path(), "src/editor.ts", "export const editor = {};\n");
    let mut selected = task("Fix calendar");
    selected.draft.description = "Keep editor compatibility while changing date boundaries.".into();
    let resolved =
        resolve_task_destinations(root.path(), &[selected], &[], &HashMap::new()).unwrap();
    assert_eq!(paths(&resolved)[0], "src/calendar.ts");
}

#[test]
fn rust_manifest_supplies_rust_module_and_test_conventions() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "Cargo.toml",
        "[package]\nname = \"notes\"\nversion = \"0.1.0\"\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Add calendar calculations")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(
        paths(&resolved)
            .iter()
            .any(|path| path.starts_with("src/") && path.ends_with(".rs"))
    );
    assert!(
        paths(&resolved)
            .iter()
            .any(|path| path.starts_with("tests/") && path.ends_with(".rs"))
    );
}

#[test]
fn rust_workspace_keeps_the_existing_crate_source_root() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/notes\"]\n",
    );
    write(
        root.path(),
        "crates/notes/src/lib.rs",
        "pub fn ready() {}\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Add calendar calculations")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(
        paths(&resolved)
            .iter()
            .any(|path| path.starts_with("crates/notes/src/"))
    );
    assert!(
        paths(&resolved)
            .iter()
            .all(|path| !path.starts_with("src/"))
    );
}

#[test]
fn manifest_only_typescript_project_keeps_its_declared_language() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "package.json",
        r#"{"dependencies":{"react":"19"},"devDependencies":{"typescript":"5","vite":"6"}}"#,
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Add calendar view")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(paths(&resolved).iter().any(|path| path.ends_with(".tsx")));
    assert!(paths(&resolved).iter().all(|path| !path.ends_with(".jsx")));
}

#[test]
fn manifest_only_rust_bootstrap_owns_the_manifest_entrypoint_module_and_test() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "Cargo.toml",
        "[package]\nname = \"notes\"\nversion = \"0.1.0\"\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Implement calendar")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    for expected in [
        "Cargo.toml",
        "src/main.rs",
        "src/calendar.rs",
        "tests/calendar.rs",
    ] {
        assert!(paths(&resolved).contains(&expected), "missing {expected}");
    }
}

#[test]
fn rust_bootstrap_honors_a_declared_custom_library_entrypoint() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "Cargo.toml",
        "[package]\nname = \"notes\"\nversion = \"0.1.0\"\n[lib]\npath = \"library/core.rs\"\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Implement calendar")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(paths(&resolved).contains(&"library/core.rs"));
    assert!(paths(&resolved).contains(&"library/calendar.rs"));
    assert!(!paths(&resolved).contains(&"src/main.rs"));
}

#[test]
fn empty_approved_react_project_owns_a_connected_application_skeleton() {
    let root = tempfile::tempdir().unwrap();
    let approved = feature("Build a React application in TypeScript.");
    let mut selected = task("Implement calendar");
    selected.draft.feature_id = Some(approved.id.clone());
    let resolved =
        resolve_task_destinations(root.path(), &[selected], &[approved], &HashMap::new()).unwrap();
    for expected in [
        "package.json",
        "index.html",
        "src/main.tsx",
        "src/App.tsx",
        "src/styles.css",
        "tests/App.test.tsx",
        "tsconfig.json",
    ] {
        assert!(paths(&resolved).contains(&expected), "missing {expected}");
    }
    assert!(paths(&resolved).len() <= 8);
}

#[test]
fn manifest_only_next_project_owns_layout_and_page_in_the_existing_app_root() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "package.json",
        r#"{"dependencies":{"next":"15"},"devDependencies":{"typescript":"5"}}"#,
    );
    std::fs::create_dir_all(root.path().join("src/app")).unwrap();
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Implement calendar")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    for expected in [
        "package.json",
        "src/app/layout.tsx",
        "src/app/page.tsx",
        "src/app/globals.css",
        "tests/calendar.test.tsx",
        "tsconfig.json",
    ] {
        assert!(paths(&resolved).contains(&expected), "missing {expected}");
    }
}

#[test]
fn manifest_only_node_project_owns_package_entrypoint_and_tests() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "package.json", r#"{"type":"module"}"#);
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Implement calendar")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    for expected in [
        "package.json",
        "src/index.js",
        "src/calendar.js",
        "tests/calendar.test.js",
    ] {
        assert!(paths(&resolved).contains(&expected), "missing {expected}");
    }
}

#[test]
fn explicit_and_manual_paths_are_retained_with_their_origin() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "src/calendar.ts",
        "export const calendar = {};\n",
    );
    let mut selected = task("Fix calendar");
    selected.draft.description = "Update src/calendar.ts".into();
    let explicit = resolve_task_destinations(
        root.path(),
        std::slice::from_ref(&selected),
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(paths(&explicit), vec!["src/calendar.ts"]);
    assert_eq!(
        explicit.task_destinations[0].origin,
        DestinationOrigin::TaskText
    );
    let manual = resolve_task_destinations(
        root.path(),
        &[selected],
        &[],
        &HashMap::from([("task-1".into(), "src/new-calendar.ts".into())]),
    )
    .unwrap();
    assert_eq!(paths(&manual), vec!["src/new-calendar.ts"]);
    assert_eq!(
        manual.task_destinations[0].origin,
        DestinationOrigin::Manual
    );
}

#[test]
fn manual_paths_reject_roots_traversal_secrets_policies_and_directories() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    for invalid in [
        "C:\\Users\\project",
        "../outside.rs",
        "src",
        ".env",
        "src/secrets.json",
        "REVIEW.md",
        "nested/REVIEW.md",
        ".hvc/notes.json",
        "target/generated.rs",
        "node_modules/module.js",
    ] {
        let hints = HashMap::from([("task-1".into(), invalid.into())]);
        assert!(
            resolve_task_destinations(root.path(), &[task("Fix calendar")], &[], &hints).is_err(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn html_and_css_files_mean_the_repository_is_not_greenfield() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "index.html", "<html></html>");
    write(root.path(), "styles.css", "body {}");
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Improve page navigation")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(!resolved.greenfield);
    assert!(paths(&resolved).contains(&"index.html"));
}

#[test]
fn truncated_repository_inventory_is_not_treated_as_empty() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..1_005 {
        write(
            root.path(),
            &format!("docs/file-{index:04}.md"),
            "planning\n",
        );
    }
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Define architecture")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(resolved.repository_truncated);
    assert!(!resolved.greenfield);
}

#[test]
fn a_hint_for_an_unselected_task_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        resolve_task_destinations(
            root.path(),
            &[task("Fix calendar")],
            &[],
            &HashMap::from([("other-task".into(), "src/calendar.rs".into())])
        )
        .is_err()
    );
}

#[test]
fn repository_config_and_source_module_names_are_not_mistaken_for_private_stores() {
    let root = tempfile::tempdir().unwrap();
    for allowed in [
        ".github/workflows/ci.yml",
        ".cargo/config.toml",
        ".gitignore",
        "src/secrets.rs",
        "src/credentials_validator.rs",
    ] {
        assert!(
            validate_destination_path(root.path(), allowed).is_ok(),
            "rejected {allowed}"
        );
    }
    write(root.path(), ".github/workflows/ci.yml", "name: CI\n");
    write(root.path(), ".cargo/config.toml", "[net]\noffline=true\n");
    assert!(validate_destination_path(root.path(), ".github/workflows/ci.yml").is_ok());
    assert!(validate_destination_path(root.path(), ".cargo/config.toml").is_ok());
}

#[test]
fn rust_binary_source_paths_are_preserved_for_task_text_and_manual_hints() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "Cargo.toml",
        "[package]\nname = \"worker\"\nversion = \"0.1.0\"\n",
    );
    write(root.path(), "src/bin/worker.rs", "fn main() {}\n");
    let mut selected = task("Fix worker startup");
    selected.draft.description = "Update src/bin/worker.rs".into();
    let explicit = resolve_task_destinations(
        root.path(),
        std::slice::from_ref(&selected),
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(paths(&explicit), vec!["src/bin/worker.rs"]);
    let manual = resolve_task_destinations(
        root.path(),
        &[selected],
        &[],
        &HashMap::from([("task-1".into(), "src/bin/worker.rs".into())]),
    )
    .unwrap();
    assert_eq!(paths(&manual), vec!["src/bin/worker.rs"]);
}

#[test]
fn observed_framework_overrides_stale_scope_language() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "package.json",
        r#"{"dependencies":{"next":"15","react":"19"}}"#,
    );
    write(
        root.path(),
        "app/layout.js",
        "export default function Layout() {}\n",
    );
    let approved = feature("Earlier exploration considered Rust, but use existing conventions.");
    let mut selected = task("Add calendar page");
    selected.draft.feature_id = Some(approved.id.clone());
    let resolved =
        resolve_task_destinations(root.path(), &[selected], &[approved], &HashMap::new()).unwrap();
    assert!(paths(&resolved).iter().any(|path| path.starts_with("app/")));
    assert!(paths(&resolved).iter().all(|path| !path.ends_with(".rs")));
}

#[test]
fn depth_limited_inventory_does_not_certify_an_empty_repository() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "a/b/c/d/e/f/g/h/i/application.ts",
        "export {};\n",
    );
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Define architecture")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(resolved.repository_truncated);
    assert!(!resolved.greenfield);
}

#[test]
fn standalone_node_initialization_uses_the_task_as_approved_architecture_input() {
    let root = tempfile::tempdir().unwrap();
    let resolved = resolve_task_destinations(
        root.path(),
        &[task("Initialize a Node application")],
        &[],
        &HashMap::new(),
    )
    .unwrap();
    assert!(paths(&resolved).contains(&"package.json"));
    assert!(paths(&resolved).contains(&"src/index.js"));
    assert!(
        paths(&resolved)
            .iter()
            .any(|path| path.starts_with("tests/") && path.ends_with(".test.js"))
    );
}

#[test]
fn observed_javascript_conventions_override_stale_typescript_scope() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "package.json",
        r#"{"dependencies":{"next":"15"}}"#,
    );
    write(
        root.path(),
        "app/layout.js",
        "export default function Layout() {}\n",
    );
    let approved = feature("Earlier exploration proposed TypeScript.");
    let mut selected = task("Add calendar page");
    selected.draft.feature_id = Some(approved.id.clone());
    let resolved =
        resolve_task_destinations(root.path(), &[selected], &[approved], &HashMap::new()).unwrap();
    assert!(paths(&resolved).contains(&"app/calendar/page.js"));
    assert!(
        paths(&resolved)
            .iter()
            .all(|path| !path.ends_with(".ts") && !path.ends_with(".tsx"))
    );
}

#[cfg(unix)]
#[test]
fn destination_cannot_cross_a_symlink_even_when_the_leaf_does_not_exist() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
    assert!(validate_destination_path(root.path(), "linked/new.rs").is_err());
}
