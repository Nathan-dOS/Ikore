fn test_catalog(path: impl Into<PathBuf>) -> CliModelCatalogService {
    CliModelCatalogService::new(path, host())
}

use super::*;
use crate::test_support::TestDatabase;
use crate::test_support::host;

#[test]
fn parses_large_opencode_verbose_catalog_with_model_specific_variants() {
    let output = (0..1_000)
        .map(|index| {
            format!(
                "provider-{}/model-{}\n{{\"name\":\"Model {}\",\"limit\":{{\"context\":128000}},\"cost\":{{\"input\":1.25,\"output\":5.0}},\"variants\":{{\"fast\":{{}},\"deep\":{{}}}},\"capabilities\":[\"tools\"]}}\n",
                index % 12,
                index,
                index
            )
        })
        .collect::<String>();
    let models = parse_opencode_verbose_models(&output).unwrap();
    assert_eq!(models.len(), 1_000);
    assert_eq!(models[0].profiles, vec!["deep", "fast"]);
    assert_eq!(models[999].context_window, Some(128_000));
}

#[test]
fn rejects_truncated_opencode_metadata() {
    let error = parse_opencode_verbose_models("openai/model\n{\"name\":")
        .expect_err("truncated metadata must fail closed");
    assert!(error.contains("invalid OpenCode metadata"));
}

#[test]
fn keeps_unknown_metadata_without_inventing_profiles() {
    let models = parse_opencode_verbose_models(
        "vendor/model\n{\"name\":\"Vendor Model\",\"unknown\":true}\n",
    )
    .unwrap();
    assert!(models[0].profiles.is_empty());
    assert_eq!(models[0].raw_metadata["unknown"], true);
}

#[test]
fn sanitize_redacted_preserves_large_catalogs_beyond_terminal_event_limit() {
    let entry = "openai/test-model\n{\"name\":\"Test Model\",\"capabilities\":[\"tools\"]}\n";
    let repeated = entry.repeat(2000);
    let sanitized = sanitize_redacted(host().as_ref(), repeated.as_bytes()).unwrap();
    assert!(!sanitized.contains("[output truncated]"));
    let models = parse_opencode_verbose_models(&sanitized).unwrap();
    assert_eq!(models.len(), 2000);
}

#[test]
fn versioned_fixtures_cover_every_cli_model_contract() {
    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/cli");
    for (backend, expected_models) in [("codex", 6), ("claude", 3), ("gemini", 3)] {
        let path = Path::new(FIXTURES)
            .join(backend)
            .join("current")
            .join("models.json");
        let value: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(value["adapterContractVersion"], ADAPTER_CONTRACT_VERSION);
        assert_eq!(
            value["models"].as_array().map(Vec::len),
            Some(expected_models),
            "unexpected model fixture size for {backend}"
        );
    }
    let opencode =
        std::fs::read_to_string(Path::new(FIXTURES).join("opencode/current/models-verbose.txt"))
            .unwrap();
    let models = parse_opencode_verbose_models(&opencode).unwrap();
    assert_eq!(models.len(), 3);
    assert_eq!(models[0].model_id.as_deref(), Some("openai/gpt-5.4"));
    assert_eq!(models[0].profiles, vec!["deep", "fast"]);
    assert_eq!(models[2].raw_metadata["experimentalMetadata"], "preserved");

    // `agy models` from 1.3.5: ten effort-suffixed slugs, four models.
    let antigravity =
        std::fs::read_to_string(Path::new(FIXTURES).join("antigravity/current/models.txt"))
            .unwrap();
    let models = parse_antigravity_models(&antigravity).unwrap();
    let summary = models
        .iter()
        .map(|model| {
            (
                model.provider_id.as_deref().unwrap(),
                model.model_id.as_deref().unwrap(),
                model.display_name.as_str(),
                model.profiles.join(","),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        vec![
            (
                "google",
                "gemini-3.8-flash",
                "Gemini 3.8 Flash",
                "low,medium,high".into()
            ),
            (
                "anthropic",
                "claude-opus-5-5",
                "Claude Opus 5.5",
                "low,medium,high".into()
            ),
            (
                "anthropic",
                "claude-sonnet-5-5",
                "Claude Sonnet 5.5",
                "low,medium,high".into()
            ),
            ("openai", "gpt-oss-120b", "GPT-OSS 120B", "medium".into()),
        ]
    );
}

#[test]
fn antigravity_catalog_keeps_unsuffixed_models_and_rejects_noise() {
    let models = parse_antigravity_models(
        "Fetching available models...\nnew-model\tNew Model\nbad slug\tBroken\nx-max\tX (Max)\n",
    )
    .unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model_id.as_deref(), Some("new-model"));
    assert!(models[0].profiles.is_empty());
    assert_eq!(models[1].model_id.as_deref(), Some("x"));
    assert_eq!(models[1].profiles, vec!["max"]);
    assert!(parse_antigravity_models("no models here").is_err());
}

#[test]
fn preset_snapshot_models_are_sourced_from_the_declarative_json_catalog() {
    for backend in [CliBackend::Codex, CliBackend::Claude, CliBackend::Gemini] {
        let snapshot = preset_snapshot("project-under-test", backend, None, None);
        let expected = declarative_models(backend);
        assert!(
            !expected.is_empty(),
            "backend {backend:?} has no catalog entries"
        );
        assert_eq!(
            snapshot.models.len(),
            expected.len() + 1,
            "backend {backend:?} should list the CLI default entry plus every catalog model"
        );
        assert_eq!(snapshot.models[1..], expected[..]);
    }
    assert!(declarative_models(CliBackend::Opencode).is_empty());
    assert!(declarative_models(CliBackend::Antigravity).is_empty());
    assert!(declarative_models(CliBackend::Custom).is_empty());
}

#[test]
fn cli_default_enrichment_parses_model_field_from_config_files() {
    let home = tempfile::tempdir().unwrap();

    // No config files yet: falls back to the plain "CLI default" label.
    assert_eq!(
        detect_cli_default_model(CliBackend::Codex, home.path()),
        None
    );
    assert_eq!(
        detect_cli_default_model(CliBackend::Claude, home.path()),
        None
    );
    assert_eq!(
        detect_cli_default_model(CliBackend::Gemini, home.path()),
        None
    );

    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    std::fs::write(
        home.path().join(".codex/config.toml"),
        "model = \"gpt-5.6-sol\"\nother_field = \"ignored\"\n",
    )
    .unwrap();
    assert_eq!(
        detect_cli_default_model(CliBackend::Codex, home.path()),
        Some("gpt-5.6-sol".into())
    );

    std::fs::write(home.path().join(".claude.json"), r#"{"model":"opus"}"#).unwrap();
    assert_eq!(
        detect_cli_default_model(CliBackend::Claude, home.path()),
        Some("opus".into())
    );

    std::fs::create_dir_all(home.path().join(".gemini")).unwrap();
    std::fs::write(
        home.path().join(".gemini/settings.json"),
        r#"{"model":"pro","apiKey":"should-never-be-read-as-model"}"#,
    )
    .unwrap();
    assert_eq!(
        detect_cli_default_model(CliBackend::Gemini, home.path()),
        Some("pro".into())
    );

    // Malformed config fails closed to `None` instead of surfacing a parse error.
    std::fs::write(home.path().join(".codex/config.toml"), "not = [valid toml").unwrap();
    assert_eq!(
        detect_cli_default_model(CliBackend::Codex, home.path()),
        None
    );

    // OpenCode/Custom never read local config.
    assert_eq!(
        detect_cli_default_model(CliBackend::Opencode, home.path()),
        None
    );
}

#[test]
fn catalog_selection_requires_a_project_model_and_compatible_profile() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Catalog validation".into()),
        )
        .unwrap();
    let service = test_catalog(persistence.database_path());
    service
        .save_snapshot(&CliModelCatalogSnapshot {
            project_id: project.id.clone(),
            backend: CliBackend::Claude,
            adapter_contract_version: ADAPTER_CONTRACT_VERSION,
            state: CliModelCatalogState::Ready,
            source: CliModelCatalogSource::Preset,
            cli_version: Some("2.1.81".into()),
            diagnostic: None,
            refreshed_at: Utc::now().to_rfc3339(),
            models: vec![
                cli_default(),
                preset("anthropic", "sonnet", "Claude Sonnet", &["low", "high"]),
            ],
        })
        .unwrap();
    let connection = Connection::open(persistence.database_path()).unwrap();

    validate_catalog_model_selection(
        &connection,
        &project.id,
        CliBackend::Claude,
        Some("sonnet"),
        Some("high"),
        CliModelSelectionSource::Catalog,
    )
    .unwrap();
    assert!(
        validate_catalog_model_selection(
            &connection,
            &project.id,
            CliBackend::Claude,
            Some("sonnet"),
            Some("ultra"),
            CliModelSelectionSource::Catalog,
        )
        .unwrap_err()
        .to_string()
        .contains("is not supported")
    );
    assert!(
        validate_catalog_model_selection(
            &connection,
            &project.id,
            CliBackend::Claude,
            Some("unverified-model"),
            Some("high"),
            CliModelSelectionSource::Manual,
        )
        .unwrap_err()
        .to_string()
        .contains("unverified manual")
    );
}

/// Project with the Antigravity catalog `agy models` listed on 2026-10-14.
fn antigravity_catalog_project() -> (tempfile::TempDir, tempfile::TempDir, TestDatabase, String) {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Antigravity effort".into()),
        )
        .unwrap();
    test_catalog(persistence.database_path())
        .save_snapshot(&CliModelCatalogSnapshot {
            project_id: project.id.clone(),
            backend: CliBackend::Antigravity,
            adapter_contract_version: ADAPTER_CONTRACT_VERSION,
            state: CliModelCatalogState::Ready,
            source: CliModelCatalogSource::Cli,
            cli_version: Some("1.3.5".into()),
            diagnostic: None,
            refreshed_at: Utc::now().to_rfc3339(),
            models: vec![
                preset("google", "gemini-3.8-flash", "Flash", &["low", "high"]),
                preset("openai", "gpt-oss-120b", "GPT-OSS 120B", &["medium"]),
                preset("google", "unsuffixed-model", "Unsuffixed", &[]),
            ],
        })
        .unwrap();
    (data, workspace, persistence, project.id)
}

#[test]
fn antigravity_effort_must_be_a_level_the_catalog_lists_for_the_model() {
    let (_data, _workspace, persistence, project) = antigravity_catalog_project();
    let connection = Connection::open(persistence.database_path()).unwrap();
    let check = |backend, model, profile, level| {
        validate_catalog_effort(&connection, &project, backend, Some(model), profile, level)
    };
    let (agy, oss) = (CliBackend::Antigravity, "gpt-oss-120b");
    let refused = check(agy, oss, None, ThinkingLevel::Low).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "invalid Agents configuration: gpt-oss-120b offers only medium through \
         Antigravity; thinking level low is not available"
    );
    assert!(check(agy, oss, None, ThinkingLevel::High).is_err());
    assert!(check(agy, "gemini-3.8-flash", None, ThinkingLevel::Medium).is_err());
    let none = check(agy, "unsuffixed-model", None, ThinkingLevel::Low).unwrap_err();
    assert!(none.to_string().contains("offers no thinking level"));
    // Offered, Default (no `--effort`), a catalog profile (it wins over the
    // level), a model the catalog lacks, and other backends all pass.
    for (backend, model, profile, level) in [
        (agy, oss, None, ThinkingLevel::Medium),
        (agy, "gemini-3.8-flash", None, ThinkingLevel::High),
        (agy, oss, None, ThinkingLevel::Default),
        (agy, oss, Some("medium"), ThinkingLevel::Low),
        (agy, "manual-model", None, ThinkingLevel::Low),
        (CliBackend::Opencode, oss, None, ThinkingLevel::Low),
    ] {
        check(backend, model, profile, level).unwrap();
    }
}
