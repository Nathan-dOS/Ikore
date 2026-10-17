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
    TerminalError, ThinkingLevel, redact_secrets, run_bounded_probe_in_with_limit,
    sanitize_terminal_text_bounded, secure_probe_spec,
};

const ADAPTER_CONTRACT_VERSION: u32 = 2;
const MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(20);
/// `agy` starts its language server (about 10 s) before it fetches the list.
const SLOW_MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(45);
/// Effort suffixes `agy models` appends to a model slug, in the order the picker shows them.
const ANTIGRAVITY_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
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
        .expect("the declarative CLI model catalog must contain valid JSON")
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
        CliBackend::Opencode | CliBackend::Antigravity | CliBackend::Custom => return Vec::new(),
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
        // OpenCode aggregates third-party providers and Antigravity lists what
        // the signed-in account may use, so both are read from the CLI itself.
        if !matches!(backend, CliBackend::Opencode | CliBackend::Antigravity) {
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

        let label = backend.label();
        let Some(availability) = availability else {
            return self.cached_or_state(
                project_id,
                backend,
                CliModelCatalogState::Missing,
                &format!("{label} CLI was not discovered"),
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
                &availability
                    .diagnostic
                    .clone()
                    .unwrap_or_else(|| format!("{label} model catalog is unavailable")),
            );
        }
        let Some(executable) = availability.executable_path.as_deref() else {
            return self.cached_or_state(
                project_id,
                backend,
                CliModelCatalogState::Missing,
                &format!("{label} executable path is unavailable"),
            );
        };

        let (args, timeout) = if backend == CliBackend::Antigravity {
            (vec!["models".to_string()], SLOW_MODEL_CATALOG_TIMEOUT)
        } else {
            let mut args = vec!["models".to_string(), "--verbose".to_string()];
            if refresh {
                args.push("--refresh".to_string());
            }
            (args, MODEL_CATALOG_TIMEOUT)
        };
        let spec = match secure_probe_spec(backend, executable, args).await {
            Ok(spec) => spec,
            Err(error) => {
                return self.cached_or_state(
                    project_id,
                    backend,
                    CliModelCatalogState::Failed,
                    &format!("{label} model catalog launch was rejected: {error}"),
                );
            }
        };
        let output = tokio::time::timeout(
            timeout,
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
                    &format!("{label} model catalog probe failed: {error}"),
                );
            }
            Err(_) => {
                return self.cached_or_state(
                    project_id,
                    backend,
                    CliModelCatalogState::Failed,
                    &format!("{label} model catalog probe timed out"),
                );
            }
        };
        if !output.status.success() {
            let diagnostic = sanitize_redacted(&output.stderr)?;
            return self.cached_or_state(
                project_id,
                backend,
                CliModelCatalogState::Failed,
                &if diagnostic.trim().is_empty() {
                    format!("{label} model catalog command failed")
                } else {
                    diagnostic.trim().to_owned()
                },
            );
        }
        let stdout = sanitize_redacted(&output.stdout)?;
        let parsed = if backend == CliBackend::Antigravity {
            parse_antigravity_models(&stdout)
        } else {
            parse_opencode_verbose_models(&stdout)
        };
        let models = match parsed {
            Ok(models) if !models.is_empty() => models,
            Ok(_) => {
                return self.cached_or_state(
                    project_id,
                    backend,
                    CliModelCatalogState::Failed,
                    &format!("{label} returned an empty model catalog"),
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

/// `agy` sends a role's or specialty's thinking level as `--effort`, and each
/// model offers its own levels (`gpt-oss-120b` only `medium` on 1.3.5); any
/// other level fails the run inside the CLI. Without a catalog profile, which
/// wins over the level, a level the project's catalog does not list for the
/// model is refused here. A model the catalog lacks (manual) is left to the CLI.
pub(crate) fn validate_catalog_effort(
    connection: &Connection,
    project_id: &str,
    backend: CliBackend,
    model: Option<&str>,
    profile: Option<&str>,
    thinking_level: ThinkingLevel,
) -> Result<(), TerminalError> {
    if backend != CliBackend::Antigravity
        || profile.is_some()
        || thinking_level == ThinkingLevel::Default
    {
        return Ok(());
    }
    let Some(model) = model else {
        return Ok(());
    };
    let Some(profiles_json) = connection
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
    else {
        return Ok(());
    };
    let offered = serde_json::from_str::<Vec<String>>(&profiles_json).map_err(|_| {
        TerminalError::Validation("the cached model profile metadata is invalid".into())
    })?;
    if offered.iter().any(|level| level == thinking_level.as_str()) {
        return Ok(());
    }
    let level = thinking_level.as_str();
    let cli = backend.label();
    Err(TerminalError::Validation(if offered.is_empty() {
        format!("{model} offers no thinking level through {cli}; use Default, not {level}")
    } else {
        format!(
            "{model} offers only {} through {cli}; thinking level {level} is not available",
            offered.join(", ")
        )
    }))
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
        CliBackend::Antigravity => {
            let contents = read_capped(&home_dir.join(".gemini/antigravity-cli/settings.json"))?;
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

/// Reads `agy models`: one `slug<TAB>label` line per model, where the slug ends in the
/// effort it runs at (`gemini-3.8-flash-low`). Variants of one model become one entry whose
/// profiles are those efforts, so the picker offers a model and then an effort - the same
/// pair `--model` and `--effort` take.
pub(crate) fn parse_antigravity_models(output: &str) -> Result<Vec<CliModelCatalogEntry>, String> {
    let mut models: Vec<CliModelCatalogEntry> = Vec::new();
    for line in output.lines() {
        let Some((slug, label)) = line.trim().split_once('\t') else {
            continue;
        };
        let (slug, label) = (slug.trim(), label.trim());
        if slug.is_empty()
            || slug.len() > 128
            || !slug
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
        {
            continue;
        }
        let (base, effort) = ANTIGRAVITY_EFFORTS
            .iter()
            .find_map(|effort| {
                slug.strip_suffix(effort)?
                    .strip_suffix('-')
                    .filter(|base| !base.is_empty())
                    .map(|base| (base, Some(*effort)))
            })
            .unwrap_or((slug, None));
        let display_name = match effort {
            Some(_) => label
                .rsplit_once(" (")
                .map_or(label, |(name, _)| name)
                .to_string(),
            None => label.to_string(),
        };
        let entry = match models
            .iter()
            .position(|entry| entry.model_id.as_deref() == Some(base))
        {
            Some(index) => &mut models[index],
            None => {
                models.push(CliModelCatalogEntry {
                    provider_id: Some(antigravity_provider(base).into()),
                    model_id: Some(base.to_string()),
                    display_name: if display_name.is_empty() {
                        base.to_string()
                    } else {
                        display_name
                    },
                    context_window: None,
                    input_cost_usd_per_million: None,
                    output_cost_usd_per_million: None,
                    capabilities: Vec::new(),
                    profiles: Vec::new(),
                    raw_metadata: Value::Object(Default::default()),
                });
                models.last_mut().expect("an entry was just pushed")
            }
        };
        if let Some(effort) = effort
            && !entry.profiles.iter().any(|profile| profile == effort)
        {
            entry.profiles.push(effort.to_string());
        }
    }
    for entry in &mut models {
        entry.profiles.sort_by_key(|profile| {
            ANTIGRAVITY_EFFORTS
                .iter()
                .position(|effort| effort == profile)
                .unwrap_or(usize::MAX)
        });
    }
    if models.is_empty() {
        return Err("agy models did not list any model".into());
    }
    Ok(models)
}

/// The vendor behind a model `agy` serves; Antigravity itself is only the gateway.
fn antigravity_provider(model: &str) -> &'static str {
    if model.starts_with("gemini") {
        "google"
    } else if model.starts_with("claude") {
        "anthropic"
    } else if model.starts_with("gpt") {
        "openai"
    } else {
        "antigravity"
    }
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
        assert!(!sanitized.contains("[output truncated]"));
        let models = parse_opencode_verbose_models(&sanitized).unwrap();
        assert_eq!(models.len(), 2000);
    }

    #[test]
    fn versioned_fixtures_cover_every_cli_model_contract() {
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

    /// Project with the Antigravity catalog `agy models` listed on 2026-10-14.
    fn antigravity_catalog_project() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PersistenceService,
        String,
    ) {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Antigravity effort".into()),
            })
            .unwrap()
            .project;
        CliModelCatalogService::new(persistence.database_path())
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

    struct NoEvents;

    impl super::super::TerminalEventSink for NoEvents {
        fn emit(&self, _envelope: &super::super::TerminalEventEnvelope) {}
    }

    /// Roles and specialties cannot save a level the model lacks; a specialty
    /// saved before its role moved to such a model is refused before any spawn.
    #[tokio::test]
    async fn antigravity_effort_the_model_lacks_is_refused_on_save_and_before_launch() {
        use crate::llm::{
            AgentExecutionTarget, AgentRole, DispatchPolicy, LlmRequest, ResponseFormat,
            UnitDispatch,
        };
        use crate::orchestration::composition::ExecutorKind;
        use crate::terminals::{
            AgentExecutionMode, SpecialtyOverride, TerminalService, UpdateCliAgentConfigRequest,
        };

        let (_data, _workspace, persistence, project) = antigravity_catalog_project();
        let terminals = std::sync::Arc::new(
            TerminalService::new(
                persistence.database_path(),
                tokio::runtime::Handle::current(),
                std::sync::Arc::new(NoEvents),
            )
            .unwrap(),
        );
        terminals
            .repository()
            .ensure_project_configs(&project, false)
            .unwrap();
        let kind = ExecutorKind::CodeImplementer;
        let role = AgentRole::for_stage(kind.stage().to_pulsation().as_db_value());
        let save_role = |model: &str, level: ThinkingLevel| {
            let current = terminals.repository().get_config(&project, role).unwrap();
            terminals.update_config(UpdateCliAgentConfigRequest {
                project_id: project.clone(),
                agent_role: role,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: CliBackend::Antigravity,
                model: Some(model.into()),
                model_profile: None,
                model_source: CliModelSelectionSource::Catalog,
                thinking_level: level,
                custom_executable: None,
                custom_args: Vec::new(),
                custom_risk_ack: false,
                expected_version: current.version,
            })
        };
        let specialty = |level| SpecialtyOverride {
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Antigravity),
            model: None,
            thinking_level: level,
        };

        let refused = save_role("gpt-oss-120b", ThinkingLevel::Low).unwrap_err();
        assert_eq!(refused.code(), "cli_configuration_invalid");
        save_role("gemini-3.8-flash", ThinkingLevel::Default).unwrap();
        persistence
            .upsert_specialty_override(&project, kind, specialty(ThinkingLevel::Low))
            .unwrap();
        save_role("gpt-oss-120b", ThinkingLevel::Default).unwrap();
        let refused = persistence
            .upsert_specialty_override(&project, kind, specialty(ThinkingLevel::High))
            .unwrap_err()
            .to_string();
        assert!(refused.contains("offers only medium"), "{refused}");

        // The `low` specialty saved while the role was on Gemini still exists.
        let request = LlmRequest {
            execution_id: "effort-execution".to_string().into(),
            generation_id: "effort-generation".to_string().into(),
            project_id: project.clone(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "effort".into(),
            target: AgentExecutionTarget::canonical(role),
            stage: "Code".into(),
            system_prompt: "Read-only".into(),
            user_prompt: "unused".into(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 100,
            temperature: 0.0,
            deadline: std::time::Instant::now() + Duration::from_secs(30),
            stream: true,
            legacy_provider: "fixture".into(),
            legacy_model: "fixture".into(),
            dispatch: DispatchPolicy {
                unit: Some(UnitDispatch {
                    stage_execution_id: "execution".into(),
                    unit_id: "unit".into(),
                    attempt_id: "attempt".into(),
                    executor_kind: kind.as_str().into(),
                    frozen_transport: "cli".into(),
                    frozen_backend: Some("antigravity".into()),
                    frozen_model: "gpt-oss-120b".into(),
                }),
                ..DispatchPolicy::default()
            },
        };
        let error = terminals
            .execute_llm_request(&request, &tokio_util::sync::CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), "cli_configuration_invalid");
        let message = error.to_string();
        assert!(
            message.contains("thinking level low is not available"),
            "{message}"
        );
        let runs: i64 = Connection::open(persistence.database_path())
            .unwrap()
            .query_row("SELECT COUNT(*) FROM terminal_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(runs, 0, "nothing was spawned or recorded");
    }
}
