use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{
    CliAvailability, CliAvailabilityState, CliBackend, MAX_MODEL_CATALOG_OUTPUT_BYTES,
    TerminalError, redact_secrets, run_bounded_probe_in_with_limit, sanitize_terminal_text_bounded,
    secure_probe_spec,
};

const ADAPTER_CONTRACT_VERSION: u32 = 2;
const MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(20);
const CLI_DEFAULT_ID: &str = "";
/// Read-only, single-field config inspection cap: the CLI default-model detector never
/// reads more than this many bytes of a local config file, and never logs their contents.
const CLI_CONFIG_READ_CAP_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliModelCatalogAsset {
    #[allow(dead_code)]
    schema_version: u32,
    catalog_version: String,
    backends: CliModelCatalogAssetBackends,
}

#[derive(Debug, Deserialize)]
struct CliModelCatalogAssetBackends {
    codex: CliModelCatalogAssetBackend,
    claude: CliModelCatalogAssetBackend,
    gemini: CliModelCatalogAssetBackend,
}

#[derive(Debug, Deserialize)]
struct CliModelCatalogAssetBackend {
    models: Vec<CliModelCatalogAssetEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliModelCatalogAssetEntry {
    provider_id: String,
    model_id: String,
    display_name: String,
    context_window: Option<u64>,
    input_cost_usd_per_million: Option<f64>,
    output_cost_usd_per_million: Option<f64>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    profiles: Vec<String>,
}

impl From<&CliModelCatalogAssetEntry> for CliModelCatalogEntry {
    fn from(entry: &CliModelCatalogAssetEntry) -> Self {
        Self {
            provider_id: Some(entry.provider_id.clone()),
            model_id: Some(entry.model_id.clone()),
            display_name: entry.display_name.clone(),
            context_window: entry.context_window,
            input_cost_usd_per_million: entry.input_cost_usd_per_million,
            output_cost_usd_per_million: entry.output_cost_usd_per_million,
            capabilities: entry.capabilities.clone(),
            profiles: entry.profiles.clone(),
            raw_metadata: Value::Object(Default::default()),
        }
    }
}

static CLI_MODEL_CATALOG: LazyLock<CliModelCatalogAsset> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../assets/cli-model-catalog-v1.json"))
        .expect("o catálogo declarativo de modelos CLI deve conter JSON válido")
});

/// Version string of the embedded declarative CLI model catalog (e.g. `"2026-09-17.1"`),
/// for display alongside the detected CLI version in the model picker header.
pub fn cli_model_catalog_version() -> &'static str {
    &CLI_MODEL_CATALOG.catalog_version
}

fn declarative_models(backend: CliBackend) -> Vec<CliModelCatalogEntry> {
    let backends = &CLI_MODEL_CATALOG.backends;
    let entries = match backend {
        CliBackend::Codex => &backends.codex.models,
        CliBackend::Claude => &backends.claude.models,
        CliBackend::Gemini => &backends.gemini.models,
        CliBackend::Opencode | CliBackend::Custom => return Vec::new(),
    };
    entries.iter().map(CliModelCatalogEntry::from).collect()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliModelSelectionSource {
    #[default]
    Default,
    Catalog,
    Manual,
}

impl CliModelSelectionSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Catalog => "catalog",
            Self::Manual => "manual",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "catalog" => Some(Self::Catalog),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliModelCatalogState {
    Ready,
    Stale,
    Missing,
    AuthRequired,
    UnsupportedVersion,
    Failed,
}

impl CliModelCatalogState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Stale => "stale",
            Self::Missing => "missing",
            Self::AuthRequired => "auth_required",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ready" => Some(Self::Ready),
            "stale" => Some(Self::Stale),
            "missing" => Some(Self::Missing),
            "auth_required" => Some(Self::AuthRequired),
            "unsupported_version" => Some(Self::UnsupportedVersion),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliModelCatalogSource {
    Preset,
    Cli,
}

impl CliModelCatalogSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Preset => "preset",
            Self::Cli => "cli",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "preset" => Some(Self::Preset),
            "cli" => Some(Self::Cli),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliModelCatalogEntry {
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub display_name: String,
    pub context_window: Option<u64>,
    pub input_cost_usd_per_million: Option<f64>,
    pub output_cost_usd_per_million: Option<f64>,
    pub capabilities: Vec<String>,
    pub profiles: Vec<String>,
    pub raw_metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliModelCatalogSnapshot {
    pub project_id: String,
    pub backend: CliBackend,
    pub adapter_contract_version: u32,
    pub state: CliModelCatalogState,
    pub source: CliModelCatalogSource,
    pub cli_version: Option<String>,
    pub diagnostic: Option<String>,
    pub refreshed_at: String,
    pub models: Vec<CliModelCatalogEntry>,
}

#[derive(Debug, Clone)]
pub struct CliModelCatalogService {
    db_path: PathBuf,
}

impl CliModelCatalogService {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    pub async fn catalog(
        &self,
        project_id: &str,
        workspace: &Path,
        backend: CliBackend,
        availability: Option<&CliAvailability>,
        refresh: bool,
    ) -> Result<CliModelCatalogSnapshot, TerminalError> {
        if backend == CliBackend::Custom {
            return Err(TerminalError::Validation(
                "Custom CLI does not expose a verified model catalog".into(),
            ));
        }
        if backend != CliBackend::Opencode {
            let home_dir = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from);
            let snapshot = preset_snapshot(project_id, backend, availability, home_dir.as_deref());
            self.save_snapshot(&snapshot)?;
            return Ok(snapshot);
        }

        if !refresh && let Some(cached) = self.load_snapshot(project_id, backend)? {
            return Ok(cached);
        }

        let Some(availability) = availability else {
            return self.cached_or_state(
                project_id,
                backend,
                CliModelCatalogState::Missing,
                "OpenCode CLI was not discovered",
            );
        };
        if availability.state != CliAvailabilityState::Available {
            let state = match availability.state {
                CliAvailabilityState::Missing => CliModelCatalogState::Missing,
                CliAvailabilityState::AuthRequired => CliModelCatalogState::AuthRequired,
                CliAvailabilityState::UnsupportedVersion
                | CliAvailabilityState::CapabilityMissing
                | CliAvailabilityState::Unsupported => CliModelCatalogState::UnsupportedVersion,
                _ => CliModelCatalogState::Failed,
            };
            return self.cached_or_state(
                project_id,
                backend,
                state,
                availability
                    .diagnostic
                    .as_deref()
                    .unwrap_or("OpenCode model catalog is unavailable"),
            );
        }
        let Some(executable) = availability.executable_path.as_deref() else {
            return self.cached_or_state(
                project_id,
                backend,
                CliModelCatalogState::Missing,
                "OpenCode executable path is unavailable",
            );
        };

        let mut args = vec!["models".to_string(), "--verbose".to_string()];
        if refresh {
            args.push("--refresh".to_string());
        }
        let spec = match secure_probe_spec(backend, executable, args).await {
            Ok(spec) => spec,
            Err(error) => {
                return self.cached_or_state(
                    project_id,
                    backend,
                    CliModelCatalogState::Failed,
                    &format!("OpenCode model catalog launch was rejected: {error}"),
                );
            }
        };
        let output = tokio::time::timeout(
            MODEL_CATALOG_TIMEOUT,
            run_bounded_probe_in_with_limit(spec, Some(workspace), MAX_MODEL_CATALOG_OUTPUT_BYTES),
        )
        .await;
        let output = match output {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return self.cached_or_state(
                    project_id,
                    backend,
                    CliModelCatalogState::Failed,
                    &format!("OpenCode model catalog probe failed: {error}"),
                );
            }
            Err(_) => {
                return self.cached_or_state(
                    project_id,
                    backend,
                    CliModelCatalogState::Failed,
                    "OpenCode model catalog probe timed out",
                );
            }
        };
        if !output.status.success() {
            let diagnostic = sanitize_redacted(&output.stderr)?;
            return self.cached_or_state(
                project_id,
                backend,
                CliModelCatalogState::Failed,
                if diagnostic.trim().is_empty() {
                    "OpenCode model catalog command failed"
                } else {
                    diagnostic.trim()
                },
            );
        }
        let stdout = sanitize_redacted(&output.stdout)?;
        let models = match parse_opencode_verbose_models(&stdout) {
            Ok(models) if !models.is_empty() => models,
            Ok(_) => {
                return self.cached_or_state(
                    project_id,
                    backend,
                    CliModelCatalogState::Failed,
                    "OpenCode returned an empty model catalog",
                );
            }
            Err(error) => {
                return self.cached_or_state(
                    project_id,
                    backend,
                    CliModelCatalogState::Failed,
                    &error,
                );
            }
        };
        let snapshot = CliModelCatalogSnapshot {
            project_id: project_id.to_string(),
            backend,
            adapter_contract_version: ADAPTER_CONTRACT_VERSION,
            state: CliModelCatalogState::Ready,
            source: CliModelCatalogSource::Cli,
            cli_version: availability.version.clone(),
            diagnostic: None,
            refreshed_at: Utc::now().to_rfc3339(),
            models: with_cli_default(models),
        };
        self.save_snapshot(&snapshot)?;
        Ok(snapshot)
    }

    pub fn cached_catalog(
        &self,
        project_id: &str,
        backend: CliBackend,
    ) -> Result<Option<CliModelCatalogSnapshot>, TerminalError> {
        self.load_snapshot(project_id, backend)
    }

    fn cached_or_state(
        &self,
        project_id: &str,
        backend: CliBackend,
        state: CliModelCatalogState,
        diagnostic: &str,
    ) -> Result<CliModelCatalogSnapshot, TerminalError> {
        if let Some(mut cached) = self.load_snapshot(project_id, backend)? {
            cached.state = CliModelCatalogState::Stale;
            cached.diagnostic = Some(diagnostic.to_string());
            return Ok(cached);
        }
        Ok(CliModelCatalogSnapshot {
            project_id: project_id.to_string(),
            backend,
            adapter_contract_version: ADAPTER_CONTRACT_VERSION,
            state,
            source: CliModelCatalogSource::Cli,
            cli_version: None,
            diagnostic: Some(diagnostic.to_string()),
            refreshed_at: Utc::now().to_rfc3339(),
            models: vec![cli_default()],
        })
    }

    fn connection(&self) -> Result<Connection, TerminalError> {
        let connection = Connection::open(&self.db_path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
    }

    fn save_snapshot(&self, snapshot: &CliModelCatalogSnapshot) -> Result<(), TerminalError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let existing_id: Option<String> = transaction
            .query_row(
                "SELECT id FROM cli_model_catalog_snapshots WHERE project_id=?1 AND backend=?2",
                params![snapshot.project_id, snapshot.backend.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let snapshot_id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        transaction.execute(
            "INSERT INTO cli_model_catalog_snapshots(
               id,project_id,backend,adapter_contract_version,state,source,cli_version,
               diagnostic,refreshed_at,created_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)
             ON CONFLICT(project_id,backend) DO UPDATE SET
               adapter_contract_version=excluded.adapter_contract_version,
               state=excluded.state,source=excluded.source,cli_version=excluded.cli_version,
               diagnostic=excluded.diagnostic,refreshed_at=excluded.refreshed_at",
            params![
                snapshot_id,
                snapshot.project_id,
                snapshot.backend.as_str(),
                snapshot.adapter_contract_version,
                snapshot.state.as_str(),
                snapshot.source.as_str(),
                snapshot.cli_version,
                snapshot.diagnostic,
                snapshot.refreshed_at,
            ],
        )?;
        transaction.execute(
            "DELETE FROM cli_model_catalog_entries WHERE snapshot_id=?1",
            [&snapshot_id],
        )?;
        for (position, model) in snapshot.models.iter().enumerate() {
            transaction.execute(
                "INSERT INTO cli_model_catalog_entries(
                   snapshot_id,provider_id,model_id,display_name,context_window,
                   input_cost_usd_per_million,output_cost_usd_per_million,
                   capabilities_json,profiles_json,raw_metadata_json,position
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    snapshot_id,
                    model.provider_id,
                    model.model_id.as_deref().unwrap_or(CLI_DEFAULT_ID),
                    model.display_name,
                    model.context_window,
                    model.input_cost_usd_per_million,
                    model.output_cost_usd_per_million,
                    serde_json::to_string(&model.capabilities)?,
                    serde_json::to_string(&model.profiles)?,
                    serde_json::to_string(&model.raw_metadata)?,
                    position,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn load_snapshot(
        &self,
        project_id: &str,
        backend: CliBackend,
    ) -> Result<Option<CliModelCatalogSnapshot>, TerminalError> {
        let connection = self.connection()?;
        let metadata = connection
            .query_row(
                "SELECT id,adapter_contract_version,state,source,cli_version,diagnostic,refreshed_at
                 FROM cli_model_catalog_snapshots WHERE project_id=?1 AND backend=?2",
                params![project_id, backend.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            adapter_contract_version,
            state,
            source,
            cli_version,
            diagnostic,
            refreshed_at,
        )) = metadata
        else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT provider_id,model_id,display_name,context_window,
                    input_cost_usd_per_million,output_cost_usd_per_million,
                    capabilities_json,profiles_json,raw_metadata_json
             FROM cli_model_catalog_entries WHERE snapshot_id=?1 ORDER BY position,model_id",
        )?;
        let models = statement
            .query_map([id], |row| {
                let model_id = row.get::<_, String>(1)?;
                let capabilities_json = row.get::<_, String>(6)?;
                let profiles_json = row.get::<_, String>(7)?;
                let raw_metadata_json = row.get::<_, String>(8)?;
                Ok(CliModelCatalogEntry {
                    provider_id: row.get(0)?,
                    model_id: (!model_id.is_empty()).then_some(model_id),
                    display_name: row.get(2)?,
                    context_window: row.get(3)?,
                    input_cost_usd_per_million: row.get(4)?,
                    output_cost_usd_per_million: row.get(5)?,
                    capabilities: serde_json::from_str(&capabilities_json).unwrap_or_default(),
                    profiles: serde_json::from_str(&profiles_json).unwrap_or_default(),
                    raw_metadata: serde_json::from_str(&raw_metadata_json)
                        .unwrap_or_else(|_| Value::Object(Default::default())),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(CliModelCatalogSnapshot {
            project_id: project_id.to_string(),
            backend,
            adapter_contract_version,
            state: CliModelCatalogState::parse(&state).unwrap_or(CliModelCatalogState::Failed),
            source: CliModelCatalogSource::parse(&source).unwrap_or(CliModelCatalogSource::Preset),
            cli_version,
            diagnostic,
            refreshed_at,
            models,
        }))
    }
}

pub(crate) fn validate_catalog_model_selection(
    connection: &Connection,
    project_id: &str,
    backend: CliBackend,
    model: Option<&str>,
    profile: Option<&str>,
    source: CliModelSelectionSource,
) -> Result<(), TerminalError> {
    match source {
        CliModelSelectionSource::Default => {
            if model.is_some() || profile.is_some() {
                return Err(TerminalError::Validation(
                    "CLI default cannot include a model ID or model profile".into(),
                ));
            }
        }
        CliModelSelectionSource::Manual => {
            if model.is_none() {
                return Err(TerminalError::Validation(
                    "manual model selection requires a model ID".into(),
                ));
            }
            if profile.is_some() {
                return Err(TerminalError::Validation(
                    "unverified manual models cannot include a model profile".into(),
                ));
            }
        }
        CliModelSelectionSource::Catalog => {
            let model = model.ok_or_else(|| {
                TerminalError::Validation("catalog model selection requires a model ID".into())
            })?;
            if backend == CliBackend::Custom {
                return Err(TerminalError::Validation(
                    "Custom CLI does not expose a verified model catalog".into(),
                ));
            }
            let profiles_json = connection
                .query_row(
                    "SELECT e.profiles_json
                     FROM cli_model_catalog_entries e
                     JOIN cli_model_catalog_snapshots s ON s.id=e.snapshot_id
                     WHERE s.project_id=?1 AND s.backend=?2 AND e.model_id=?3
                     LIMIT 1",
                    params![project_id, backend.as_str(), model],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    TerminalError::Validation(format!(
                        "model {model} is not present in the project-scoped {} catalog",
                        backend.label()
                    ))
                })?;
            if let Some(profile) = profile {
                let profiles =
                    serde_json::from_str::<Vec<String>>(&profiles_json).map_err(|_| {
                        TerminalError::Validation(
                            "the cached model profile metadata is invalid".into(),
                        )
                    })?;
                if !profiles.iter().any(|candidate| candidate == profile) {
                    return Err(TerminalError::Validation(format!(
                        "model profile {profile} is not supported by {model}"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn sanitize_redacted(bytes: &[u8]) -> Result<String, TerminalError> {
    redact_secrets(&sanitize_terminal_text_bounded(
        bytes,
        MAX_MODEL_CATALOG_OUTPUT_BYTES,
    ))
    .map(|result| result.text)
    .map_err(|error| TerminalError::Security(error.to_string()))
}

fn preset_snapshot(
    project_id: &str,
    backend: CliBackend,
    availability: Option<&CliAvailability>,
    home_dir: Option<&Path>,
) -> CliModelCatalogSnapshot {
    let (state, diagnostic) = match availability {
        Some(item) if item.state == CliAvailabilityState::Available => {
            (CliModelCatalogState::Ready, None)
        }
        Some(item) => (
            match item.state {
                CliAvailabilityState::Missing => CliModelCatalogState::Missing,
                CliAvailabilityState::AuthRequired => CliModelCatalogState::AuthRequired,
                CliAvailabilityState::UnsupportedVersion
                | CliAvailabilityState::CapabilityMissing
                | CliAvailabilityState::Unsupported => CliModelCatalogState::UnsupportedVersion,
                _ => CliModelCatalogState::Failed,
            },
            item.diagnostic.clone(),
        ),
        None => (
            CliModelCatalogState::Missing,
            Some(format!("{} CLI was not discovered", backend.label())),
        ),
    };
    let default_hint = home_dir.and_then(|dir| detect_cli_default_model(backend, dir));
    let mut models = vec![cli_default_with_hint(default_hint)];
    models.extend(declarative_models(backend));
    CliModelCatalogSnapshot {
        project_id: project_id.to_string(),
        backend,
        adapter_contract_version: ADAPTER_CONTRACT_VERSION,
        state,
        source: CliModelCatalogSource::Preset,
        cli_version: availability.and_then(|item| item.version.clone()),
        diagnostic,
        refreshed_at: Utc::now().to_rfc3339(),
        models,
    }
}

fn cli_default() -> CliModelCatalogEntry {
    cli_default_with_hint(None)
}

fn cli_default_with_hint(detected_model: Option<String>) -> CliModelCatalogEntry {
    let display_name = match detected_model {
        Some(model) => format!("CLI default (configured: {model})"),
        None => "CLI default".into(),
    };
    CliModelCatalogEntry {
        provider_id: None,
        model_id: None,
        display_name,
        context_window: None,
        input_cost_usd_per_million: None,
        output_cost_usd_per_million: None,
        capabilities: Vec::new(),
        profiles: Vec::new(),
        raw_metadata: Value::Object(Default::default()),
    }
}

/// Reads only the `model` field from the CLI's own local config file to enrich the
/// "CLI default" catalog entry label. Read-only, single-field, capped at
/// [`CLI_CONFIG_READ_CAP_BYTES`], never logs or persists the file's contents, and fails
/// closed to `None` on any error (missing file, unreadable, malformed, field absent).
/// This is not a catalog refresh: no network access, no CLI invocation, no side effect.
fn detect_cli_default_model(backend: CliBackend, home_dir: &Path) -> Option<String> {
    match backend {
        CliBackend::Codex => {
            let contents = read_capped(&home_dir.join(".codex/config.toml"))?;
            let value: toml::Value = toml::from_str(&contents).ok()?;
            value.get("model")?.as_str().map(str::to_string)
        }
        CliBackend::Claude => {
            for candidate in [
                home_dir.join(".claude/settings.json"),
                home_dir.join(".claude.json"),
            ] {
                if let Some(contents) = read_capped(&candidate)
                    && let Ok(value) = serde_json::from_str::<Value>(&contents)
                    && let Some(model) = value.get("model").and_then(Value::as_str)
                {
                    return Some(model.to_string());
                }
            }
            None
        }
        CliBackend::Gemini => {
            let contents = read_capped(&home_dir.join(".gemini/settings.json"))?;
            let value: Value = serde_json::from_str(&contents).ok()?;
            value.get("model")?.as_str().map(str::to_string)
        }
        CliBackend::Opencode | CliBackend::Custom => None,
    }
}

fn read_capped(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > CLI_CONFIG_READ_CAP_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
fn preset(
    provider: &str,
    model: &str,
    display_name: &str,
    profiles: &[&str],
) -> CliModelCatalogEntry {
    CliModelCatalogEntry {
        provider_id: Some(provider.into()),
        model_id: Some(model.into()),
        display_name: display_name.into(),
        context_window: None,
        input_cost_usd_per_million: None,
        output_cost_usd_per_million: None,
        capabilities: Vec::new(),
        profiles: profiles.iter().map(|value| (*value).to_string()).collect(),
        raw_metadata: Value::Object(Default::default()),
    }
}

fn with_cli_default(mut models: Vec<CliModelCatalogEntry>) -> Vec<CliModelCatalogEntry> {
    models.sort_by(|left, right| {
        left.provider_id
            .cmp(&right.provider_id)
            .then_with(|| left.display_name.cmp(&right.display_name))
    });
    models.insert(0, cli_default());
    models
}

pub(crate) fn parse_opencode_verbose_models(
    output: &str,
) -> Result<Vec<CliModelCatalogEntry>, String> {
    let mut models = Vec::new();
    let mut offset = 0usize;
    while offset < output.len() {
        let Some(relative_end) = output[offset..].find('\n') else {
            break;
        };
        let line_end = offset + relative_end;
        let candidate = output[offset..line_end].trim();
        let metadata_start = line_end + 1;
        offset = metadata_start;
        if !is_opencode_model_reference(candidate) {
            continue;
        }
        let tail = output[metadata_start..].trim_start();
        let whitespace = output[metadata_start..].len() - tail.len();
        let mut values = serde_json::Deserializer::from_str(tail).into_iter::<Value>();
        let metadata = values
            .next()
            .transpose()
            .map_err(|error| format!("invalid OpenCode metadata for {candidate}: {error}"))?
            .ok_or_else(|| format!("missing OpenCode metadata for {candidate}"))?;
        offset = metadata_start + whitespace + values.byte_offset();
        models.push(normalize_opencode_model(candidate, metadata));
    }
    if models.is_empty() {
        return Err("OpenCode verbose output did not contain any provider/model entries".into());
    }
    Ok(models)
}

fn is_opencode_model_reference(value: &str) -> bool {
    let Some((provider, model)) = value.split_once('/') else {
        return false;
    };
    !provider.is_empty()
        && !model.is_empty()
        && value.len() <= 512
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "/._:@+-".contains(character))
}

fn normalize_opencode_model(reference: &str, metadata: Value) -> CliModelCatalogEntry {
    let (provider, _) = reference.split_once('/').unwrap_or_default();
    let display_name = metadata
        .get("name")
        .or_else(|| metadata.get("displayName"))
        .and_then(Value::as_str)
        .unwrap_or(reference)
        .to_string();
    let context_window = metadata
        .pointer("/limit/context")
        .or_else(|| metadata.get("contextWindow"))
        .and_then(Value::as_u64);
    let input_cost = metadata.pointer("/cost/input").and_then(Value::as_f64);
    let output_cost = metadata.pointer("/cost/output").and_then(Value::as_f64);
    let mut profiles: Vec<String> = metadata
        .get("variants")
        .and_then(Value::as_object)
        .map(|variants| variants.keys().cloned().collect())
        .unwrap_or_default();
    profiles.sort();
    profiles.dedup();
    let mut capabilities = metadata
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(modalities) = metadata.get("modalities").and_then(Value::as_object) {
        capabilities.extend(modalities.keys().cloned());
    }
    capabilities.sort();
    capabilities.dedup();
    CliModelCatalogEntry {
        provider_id: Some(provider.to_string()),
        model_id: Some(reference.to_string()),
        display_name,
        context_window,
        input_cost_usd_per_million: input_cost,
        output_cost_usd_per_million: output_cost,
        capabilities,
        profiles,
        raw_metadata: metadata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence_service::{CreateProjectRequest, PersistenceService};

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
        let sanitized = sanitize_redacted(repeated.as_bytes()).unwrap();
        assert!(!sanitized.contains("[saída truncada]"));
        let models = parse_opencode_verbose_models(&sanitized).unwrap();
        assert_eq!(models.len(), 2000);
    }

    #[test]
    fn versioned_fixtures_cover_all_four_cli_model_contracts() {
        const FIXTURES: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../hvc-test-support/fixtures/cli"
        );
        for (backend, expected_models) in [("codex", 6), ("claude", 3), ("gemini", 3)] {
            let path = Path::new(FIXTURES)
                .join(backend)
                .join("current")
                .join("models.json");
            let value: Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(value["adapterContractVersion"], ADAPTER_CONTRACT_VERSION);
            assert_eq!(
                value["models"].as_array().map(Vec::len),
                Some(expected_models),
                "unexpected model fixture size for {backend}"
            );
        }
        let opencode = std::fs::read_to_string(
            Path::new(FIXTURES).join("opencode/current/models-verbose.txt"),
        )
        .unwrap();
        let models = parse_opencode_verbose_models(&opencode).unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].model_id.as_deref(), Some("openai/gpt-5.4"));
        assert_eq!(models[0].profiles, vec!["deep", "fast"]);
        assert_eq!(models[2].raw_metadata["experimentalMetadata"], "preserved");
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
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Catalog validation".into()),
            })
            .unwrap()
            .project;
        let service = CliModelCatalogService::new(persistence.database_path());
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
}
