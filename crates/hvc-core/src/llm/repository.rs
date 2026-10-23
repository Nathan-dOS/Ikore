use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::catalog::{model_entry, model_price_basis};
use super::compression::CompressionPreview;
use super::credentials::CredentialStore;
use super::domain::{
    AgentExecutionTarget, AgentRole, CandidateDecision, FreeFormProviderRouteBinding,
    LlmExecutionListItem, ProviderConnection, ProviderRoute, ProviderRouteBinding,
    ResolvedConnection, RouteDecision, RouteTarget,
};
use super::error::{LlmError, LlmErrorKind};
use super::profiles::{profile_by_id, profiles};
use super::scoring::{ScoreInput, normalize_metric, score};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertProviderConnectionRequest {
    pub id: Option<String>,
    pub provider_id: String,
    pub display_name: String,
    pub endpoint_url: Option<String>,
    pub default_model: String,
    pub enabled: Option<bool>,
    pub max_concurrency: Option<u32>,
    pub api_key: Option<String>,
    pub version: Option<u64>,
}

impl std::fmt::Debug for UpsertProviderConnectionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpsertProviderConnectionRequest")
            .field("id", &self.id)
            .field("provider_id", &self.provider_id)
            .field("display_name", &self.display_name)
            .field("endpoint_url", &self.endpoint_url)
            .field("default_model", &self.default_model)
            .field("enabled", &self.enabled)
            .field("max_concurrency", &self.max_concurrency)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("version", &self.version)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProviderConnectionRequest {
    pub connection_id: String,
    pub version: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestProviderConnectionRequest {
    pub connection_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestProviderConnectionResponse {
    pub connection_id: String,
    pub reachable: bool,
    pub provider_id: String,
    pub model: String,
    pub latency_ms: u64,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertProviderRouteRequest {
    pub id: Option<String>,
    pub name: String,
    pub strategy: String,
    pub max_attempts: Option<u32>,
    pub attempt_timeout_ms: Option<u64>,
    pub route_timeout_ms: Option<u64>,
    pub enabled: Option<bool>,
    pub targets: Vec<RouteTarget>,
    pub version: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProviderRouteRequest {
    pub route_id: String,
    pub version: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertProviderRouteBindingRequest {
    pub project_id: String,
    pub agent_role: String,
    pub stage: String,
    pub route_id: String,
    pub enabled: Option<bool>,
    pub version: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertFreeFormProviderRouteBindingRequest {
    pub project_id: String,
    pub slot_id: String,
    pub route_id: String,
    pub enabled: Option<bool>,
    pub version: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetLlmResilienceStateRequest {
    pub connection_id: Option<String>,
    pub provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResilienceStateItem {
    pub scope: String,
    pub scope_id: String,
    pub state: String,
    pub retry_at: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmResilienceSnapshot {
    pub generated_at: String,
    pub states: Vec<ResilienceStateItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmExecutionSnapshot {
    pub execution_id: String,
    pub generation_id: String,
    pub route_id: String,
    pub status: String,
    pub decision: Value,
    pub next_eligible_at: Option<String>,
    pub visible_output: Option<String>,
    pub visible_output_complete: bool,
    pub attempts: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmRoutingMetric {
    pub provider_id: String,
    pub model_id: String,
    pub attempts: u64,
    pub successes: u64,
    pub average_latency_ms: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ResolvedRouteTarget {
    pub target: RouteTarget,
    pub connection: ResolvedConnection,
    pub provider_local: bool,
}

pub trait LlmRepository: Send + Sync {
    fn resolve_route(
        &self,
        project_id: &str,
        role: AgentRole,
        stage: &str,
    ) -> Result<ProviderRoute, LlmError>;
    fn resolved_targets(&self, route_id: &str) -> Result<Vec<ResolvedRouteTarget>, LlmError>;
    fn decide(
        &self,
        route: &ProviderRoute,
        targets: &[ResolvedRouteTarget],
        affinity_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<RouteDecision, LlmError>;
}

#[derive(Debug, Clone)]
pub struct SqliteLlmRepository {
    db_path: PathBuf,
}

impl SqliteLlmRepository {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: db_path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, LlmError> {
        let connection = Connection::open(&self.db_path).map_err(db_error)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(db_error)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_error)?;
        Ok(connection)
    }

    pub fn ensure_legacy_configuration(&self) -> Result<(), LlmError> {
        let connection = self.connection()?;
        let now = Utc::now().to_rfc3339();
        let legacy_provider = connection
            .query_row(
                "SELECT value FROM runtime_settings WHERE key = 'runtime.provider'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or_else(|| "openai".to_string());
        let legacy_model = connection
            .query_row(
                "SELECT value FROM runtime_settings WHERE key = 'runtime.model'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or_else(|| "gpt-4.1-mini".to_string());

        connection.execute(
            "INSERT INTO provider_connections (
               id, provider_id, display_name, credential_ref, endpoint_url, default_model,
               enabled, state, max_concurrency, version, created_at, updated_at
             ) VALUES ('legacy-openai', 'openai_legacy', 'OpenAI legado', 'openai', NULL, ?1, 1, 'active', 1, 1, ?2, ?2)
             ON CONFLICT(id) DO NOTHING",
            params![if legacy_provider == "openai" { legacy_model.as_str() } else { "gpt-4.1-mini" }, now],
        ).map_err(db_error)?;
        connection.execute(
            "INSERT INTO provider_connections (
               id, provider_id, display_name, credential_ref, endpoint_url, default_model,
               enabled, state, max_concurrency, version, created_at, updated_at
             ) VALUES ('legacy-anthropic', 'anthropic', 'Anthropic legado', 'anthropic', NULL, ?1, 1, 'active', 1, 1, ?2, ?2)
             ON CONFLICT(id) DO NOTHING",
            params![if legacy_provider == "anthropic" { legacy_model.as_str() } else { "claude-3-5-sonnet-latest" }, now],
        ).map_err(db_error)?;
        connection.execute(
            "INSERT INTO provider_routes (
               id, name, strategy, max_attempts, attempt_timeout_ms, route_timeout_ms,
               enabled, version, created_at, updated_at
             ) VALUES ('legacy-default', 'Legacy default', 'priority', 3, 30000, 45000, 1, 1, ?1, ?1)
             ON CONFLICT(id) DO NOTHING",
            params![now],
        ).map_err(db_error)?;

        let target_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM provider_route_targets WHERE route_id = 'legacy-default'",
                [],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if target_count == 0 {
            let order = if legacy_provider == "anthropic" {
                [
                    ("legacy-anthropic", "claude-3-5-sonnet-latest"),
                    ("legacy-openai", "gpt-4.1-mini"),
                ]
            } else {
                [
                    ("legacy-openai", "gpt-4.1-mini"),
                    ("legacy-anthropic", "claude-3-5-sonnet-latest"),
                ]
            };
            for (position, (connection_id, fallback_model)) in order.into_iter().enumerate() {
                let model = if (legacy_provider == "openai" && connection_id == "legacy-openai")
                    || (legacy_provider == "anthropic" && connection_id == "legacy-anthropic")
                {
                    legacy_model.as_str()
                } else {
                    fallback_model
                };
                connection
                    .execute(
                        "INSERT INTO provider_route_targets (
                       route_id, position, connection_id, model_id, enabled, created_at, updated_at
                     ) VALUES ('legacy-default', ?1, ?2, ?3, 1, ?4, ?4)",
                        params![position as i64, connection_id, model, now],
                    )
                    .map_err(db_error)?;
            }
        }
        Ok(())
    }

    pub fn routing_mode(&self) -> Result<String, LlmError> {
        self.connection()?
            .query_row(
                "SELECT value FROM runtime_settings WHERE key='runtime.routing_v2_mode'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)
            .map(|value| value.unwrap_or_else(|| "on".to_string()))
    }

    pub fn set_routing_mode(&self, mode: &str) -> Result<(), LlmError> {
        if !matches!(mode, "off" | "shadow" | "on") {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "routingV2Mode must be off, shadow or on.",
            ));
        }
        self.connection()?.execute(
            "INSERT INTO runtime_settings(key,value,updated_at) VALUES ('runtime.routing_v2_mode',?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
            params![mode, Utc::now().to_rfc3339()],
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn list_connections(
        &self,
        credentials: &dyn CredentialStore,
    ) -> Result<Vec<ProviderConnection>, LlmError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, provider_id, display_name, credential_ref, endpoint_url, default_model,
                    enabled, state, blocked_reason, cooldown_until, max_concurrency, version,
                    created_at, updated_at
             FROM provider_connections ORDER BY display_name COLLATE NOCASE, id",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, u32>(10)?,
                    row.get::<_, u64>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                ))
            })
            .map_err(db_error)?;
        let mut items = Vec::new();
        for row in rows {
            let (
                id,
                provider_id,
                display_name,
                credential_ref,
                endpoint_url,
                default_model,
                enabled,
                state,
                blocked_reason,
                cooldown_until,
                max_concurrency,
                version,
                created_at,
                updated_at,
            ) = row.map_err(db_error)?;
            let profile = profile_by_id(&provider_id);
            let has_api_key = profile.as_ref().is_some_and(|profile| profile.local)
                || credentials.get(&credential_ref).ok().flatten().is_some();
            let chat_eligible = enabled
                && state != "blocked"
                && has_api_key
                && profile
                    .as_ref()
                    .is_some_and(|profile| profile.capabilities.tool_calling)
                // Only OpenRouter's per-model gate is locally curated. Other API providers may
                // publish a model through their explicit per-connection catalog without a
                // duplicate entry in the static pricing/capability asset.
                && (provider_id != "openrouter"
                    || model_entry(&provider_id, &default_model)
                        .is_some_and(|model| model.tool_calling));
            items.push(ProviderConnection {
                id,
                provider_id,
                display_name,
                endpoint_url,
                default_model,
                enabled,
                state,
                blocked_reason,
                cooldown_until,
                max_concurrency,
                version,
                has_api_key,
                chat_eligible,
                created_at,
                updated_at,
            });
        }
        Ok(items)
    }

    pub fn upsert_connection(
        &self,
        credentials: &dyn CredentialStore,
        request: UpsertProviderConnectionRequest,
    ) -> Result<ProviderConnection, LlmError> {
        let profile = profile_by_id(request.provider_id.trim()).ok_or_else(|| {
            LlmError::new(
                LlmErrorKind::InvalidRequest,
                "Provider profile desconhecido.",
            )
        })?;
        if request.provider_id == "openai_legacy" && request.id.as_deref() != Some("legacy-openai")
        {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "Legacy OpenAI is reserved for the legacy-openai connection.",
            ));
        }
        let display_name = non_empty(&request.display_name, "displayName")?;
        let default_model = non_empty(&request.default_model, "defaultModel")?;
        if profile.id == "openrouter" && model_entry(&profile.id, default_model).is_none() {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "OpenRouter model ID is not in the curated catalog.",
            ));
        }
        let endpoint_url = if profile.endpoint_configurable {
            let resolved = profile.resolved_endpoint(request.endpoint_url.as_deref())?;
            Some(
                resolved
                    .trim_end_matches("/v1/chat/completions")
                    .trim_end_matches('/')
                    .to_string(),
            )
        } else {
            None
        };
        let id = request.id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let credential_ref = format!("connection:{id}");
        let now = Utc::now().to_rfc3339();
        let existing = self.find_raw_connection(&id)?;
        let actual_credential_ref = existing
            .as_ref()
            .map(|item| item.2.clone())
            .unwrap_or(credential_ref);

        if let Some((actual_version, _, _)) = existing.as_ref() {
            let expected = request
                .version
                .ok_or_else(|| version_conflict(0, *actual_version))?;
            if expected != *actual_version {
                return Err(version_conflict(expected, *actual_version));
            }
        }

        if let Some(api_key) = request.api_key.as_deref() {
            credentials.set(&actual_credential_ref, api_key)?;
        }

        let connection = self.connection()?;
        if let Some((actual_version, previous_provider_id, _)) = existing {
            let expected = request
                .version
                .ok_or_else(|| version_conflict(0, actual_version))?;
            if expected != actual_version {
                return Err(version_conflict(expected, actual_version));
            }
            let changed = connection
                .execute(
                    "UPDATE provider_connections
                 SET provider_id=?1, display_name=?2, endpoint_url=?3, default_model=?4,
                     enabled=?5, max_concurrency=?6, version=version+1, updated_at=?7
                 WHERE id=?8 AND version=?9",
                    params![
                        profile.id,
                        display_name,
                        endpoint_url,
                        default_model,
                        request.enabled.unwrap_or(true),
                        request.max_concurrency.unwrap_or(1).clamp(1, 32),
                        now,
                        id,
                        expected
                    ],
                )
                .map_err(db_error)?;
            if changed == 0 {
                return Err(version_conflict(expected, actual_version));
            }
            // A snapshot is valid only for the provider adapter that published it.
            if previous_provider_id != profile.id {
                connection
                    .execute(
                        "DELETE FROM api_model_catalog_snapshots WHERE connection_id=?1",
                        params![id],
                    )
                    .map_err(db_error)?;
            }
        } else {
            connection
                .execute(
                    "INSERT INTO provider_connections (
                   id, provider_id, display_name, credential_ref, endpoint_url, default_model,
                   enabled, state, max_concurrency, version, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, 1, ?9, ?9)",
                    params![
                        id,
                        profile.id,
                        display_name,
                        actual_credential_ref,
                        endpoint_url,
                        default_model,
                        request.enabled.unwrap_or(true),
                        request.max_concurrency.unwrap_or(1).clamp(1, 32),
                        now
                    ],
                )
                .map_err(db_error)?;
        }
        self.list_connections(credentials)?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| {
                LlmError::new(
                    LlmErrorKind::Persistence,
                    "The saved connection was not found.",
                )
            })
    }

    pub fn delete_connection(
        &self,
        credentials: &dyn CredentialStore,
        request: DeleteProviderConnectionRequest,
    ) -> Result<bool, LlmError> {
        if request.connection_id.starts_with("legacy-") {
            return Err(LlmError::new(
                LlmErrorKind::RouteConflict,
                "Legacy connections cannot be deleted.",
            ));
        }
        let (actual_version, _, credential_ref) = self
            .find_raw_connection(&request.connection_id)?
            .ok_or_else(|| LlmError::new(LlmErrorKind::InvalidRequest, "Connection not found."))?;
        if actual_version != request.version {
            return Err(version_conflict(request.version, actual_version));
        }
        let connection = self.connection()?;
        let references: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM provider_route_targets WHERE connection_id=?1",
                params![request.connection_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if references > 0 {
            return Err(LlmError::new(
                LlmErrorKind::RouteConflict,
                "Remove the connection from its routes before deleting it.",
            ));
        }
        credentials.delete(&credential_ref)?;
        let changed = connection
            .execute(
                "DELETE FROM provider_connections WHERE id=?1 AND version=?2",
                params![request.connection_id, request.version],
            )
            .map_err(db_error)?;
        Ok(changed > 0)
    }

    pub fn list_routes(&self) -> Result<Vec<ProviderRoute>, LlmError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, strategy, max_attempts, attempt_timeout_ms, route_timeout_ms,
                    enabled, version, created_at, updated_at
             FROM provider_routes ORDER BY name COLLATE NOCASE, id",
            )
            .map_err(db_error)?;
        let routes = statement
            .query_map([], |row| {
                Ok(ProviderRoute {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    strategy: row.get(2)?,
                    max_attempts: row.get(3)?,
                    attempt_timeout_ms: row.get(4)?,
                    route_timeout_ms: row.get(5)?,
                    enabled: row.get(6)?,
                    version: row.get(7)?,
                    targets: Vec::new(),
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })
            .map_err(db_error)?;
        let mut result = Vec::new();
        for route in routes {
            let mut route = route.map_err(db_error)?;
            route.targets = self.targets_for_route(&connection, &route.id)?;
            result.push(route);
        }
        Ok(result)
    }

    fn targets_for_route(
        &self,
        connection: &Connection,
        route_id: &str,
    ) -> Result<Vec<RouteTarget>, LlmError> {
        let mut statement = connection
            .prepare(
                "SELECT position, connection_id, model_id, enabled
             FROM provider_route_targets WHERE route_id=?1 ORDER BY position",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![route_id], |row| {
                Ok(RouteTarget {
                    position: row.get(0)?,
                    connection_id: row.get(1)?,
                    model_id: row.get(2)?,
                    enabled: row.get(3)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn upsert_route(
        &self,
        request: UpsertProviderRouteRequest,
    ) -> Result<ProviderRoute, LlmError> {
        let name = non_empty(&request.name, "name")?;
        if !matches!(request.strategy.as_str(), "priority" | "lkgp" | "score") {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "strategy must be priority, lkgp or score.",
            ));
        }
        if request.targets.is_empty() {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "The route needs at least one target.",
            ));
        }
        let mut targets = request.targets;
        targets.sort_by_key(|target| target.position);
        for (expected, target) in targets.iter().enumerate() {
            if target.position != expected as u32 {
                return Err(LlmError::new(
                    LlmErrorKind::InvalidRequest,
                    "Route positions must be contiguous from zero.",
                ));
            }
        }
        let id = request.id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut connection = self.connection()?;
        // Reserve the writer before reading the current version. A deferred
        // transaction can otherwise lose the read-to-write upgrade race to the
        // workspace maintenance worker and report SQLITE_BUSY immediately,
        // bypassing the configured busy timeout.
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let actual_version = transaction
            .query_row(
                "SELECT version FROM provider_routes WHERE id=?1",
                params![id],
                |row| row.get::<_, u64>(0),
            )
            .optional()
            .map_err(db_error)?;
        let now = Utc::now().to_rfc3339();
        if let Some(actual) = actual_version {
            if id == "legacy-default" && request.name != "Legacy default" {
                return Err(LlmError::new(
                    LlmErrorKind::RouteConflict,
                    "The legacy-default route cannot be renamed.",
                ));
            }
            let expected = request.version.ok_or_else(|| version_conflict(0, actual))?;
            if expected != actual {
                return Err(version_conflict(expected, actual));
            }
            transaction
                .execute(
                    "UPDATE provider_routes SET name=?1, strategy=?2, max_attempts=?3,
                 attempt_timeout_ms=?4, route_timeout_ms=?5, enabled=?6,
                 version=version+1, updated_at=?7 WHERE id=?8 AND version=?9",
                    params![
                        name,
                        request.strategy,
                        request.max_attempts.unwrap_or(3).clamp(1, 10),
                        request
                            .attempt_timeout_ms
                            .unwrap_or(30000)
                            .clamp(1000, 300000),
                        request
                            .route_timeout_ms
                            .unwrap_or(45000)
                            .clamp(1000, 600000),
                        request.enabled.unwrap_or(true),
                        now,
                        id,
                        expected
                    ],
                )
                .map_err(db_error)?;
            transaction
                .execute(
                    "DELETE FROM provider_route_targets WHERE route_id=?1",
                    params![id],
                )
                .map_err(db_error)?;
        } else {
            transaction.execute(
                "INSERT INTO provider_routes (id,name,strategy,max_attempts,attempt_timeout_ms,route_timeout_ms,enabled,version,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,1,?8,?8)",
                params![id,name,request.strategy,request.max_attempts.unwrap_or(3).clamp(1,10),request.attempt_timeout_ms.unwrap_or(30000).clamp(1000,300000),request.route_timeout_ms.unwrap_or(45000).clamp(1000,600000),request.enabled.unwrap_or(true),now]
            ).map_err(db_error)?;
        }
        for target in targets {
            transaction.execute(
                "INSERT INTO provider_route_targets (route_id,position,connection_id,model_id,enabled,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?6)",
                params![id,target.position,target.connection_id,non_empty(&target.model_id,"modelId")?,target.enabled,now]
            ).map_err(|error| foreign_key_or_db(error, "A target references a connection that does not exist."))?;
        }
        transaction.commit().map_err(db_error)?;
        self.list_routes()?
            .into_iter()
            .find(|route| route.id == id)
            .ok_or_else(|| {
                LlmError::new(LlmErrorKind::Persistence, "The saved route was not found.")
            })
    }

    pub fn delete_route(&self, request: DeleteProviderRouteRequest) -> Result<bool, LlmError> {
        if request.route_id == "legacy-default" {
            return Err(LlmError::new(
                LlmErrorKind::RouteConflict,
                "The legacy-default route cannot be deleted.",
            ));
        }
        let connection = self.connection()?;
        let actual = connection
            .query_row(
                "SELECT version FROM provider_routes WHERE id=?1",
                params![request.route_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| LlmError::new(LlmErrorKind::InvalidRequest, "Route not found."))?;
        if actual != request.version {
            return Err(version_conflict(request.version, actual));
        }
        let references: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM provider_route_bindings WHERE route_id=?1",
                params![request.route_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if references > 0 {
            return Err(LlmError::new(
                LlmErrorKind::RouteConflict,
                "Remove the bindings before deleting the route.",
            ));
        }
        connection
            .execute(
                "DELETE FROM provider_route_targets WHERE route_id=?1",
                params![request.route_id],
            )
            .map_err(db_error)?;
        Ok(connection
            .execute(
                "DELETE FROM provider_routes WHERE id=?1 AND version=?2",
                params![request.route_id, request.version],
            )
            .map_err(db_error)?
            > 0)
    }

    pub fn list_bindings(&self) -> Result<Vec<ProviderRouteBinding>, LlmError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT project_id,agent_role,stage,route_id,enabled,version,created_at,updated_at
             FROM provider_route_bindings ORDER BY project_id,agent_role,stage",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(ProviderRouteBinding {
                    project_id: row.get(0)?,
                    agent_role: row.get(1)?,
                    stage: row.get(2)?,
                    route_id: row.get(3)?,
                    enabled: row.get(4)?,
                    version: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn upsert_binding(
        &self,
        mut request: UpsertProviderRouteBindingRequest,
    ) -> Result<ProviderRouteBinding, LlmError> {
        non_empty(&request.project_id, "projectId")?;
        non_empty(&request.agent_role, "agentRole")?;
        non_empty(&request.stage, "stage")?;
        non_empty(&request.route_id, "routeId")?;
        request.stage = crate::orchestration::catalog::canonical_stage(&request.stage).to_owned();
        let mut db = self.connection()?;
        let connection = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let actual=connection.query_row(
            "SELECT version FROM provider_route_bindings WHERE project_id=?1 AND agent_role=?2 AND stage=?3",
            params![request.project_id,request.agent_role,request.stage],|row|row.get::<_,u64>(0)
        ).optional().map_err(db_error)?;
        let now = Utc::now().to_rfc3339();
        if let Some(actual) = actual {
            let expected = request.version.ok_or_else(|| version_conflict(0, actual))?;
            if expected != actual {
                return Err(version_conflict(expected, actual));
            }
            connection.execute(
                "UPDATE provider_route_bindings SET route_id=?1,enabled=?2,version=version+1,updated_at=?3
                 WHERE project_id=?4 AND agent_role=?5 AND stage=?6 AND version=?7",
                params![request.route_id,request.enabled.unwrap_or(true),now,request.project_id,request.agent_role,request.stage,expected]
            ).map_err(|error|foreign_key_or_db(error,"The binding references a project or route that does not exist."))?;
        } else {
            connection.execute(
                "INSERT INTO provider_route_bindings (project_id,agent_role,stage,route_id,enabled,version,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,1,?6,?6)",
                params![request.project_id,request.agent_role,request.stage,request.route_id,request.enabled.unwrap_or(true),now]
            ).map_err(|error|foreign_key_or_db(error,"The binding references a project or route that does not exist."))?;
        }
        if request.stage == "SPECIFICATION" {
            // An explicit versioned save is the human resolution of a migrated alias collision.
            connection.execute("DELETE FROM provider_route_bindings WHERE project_id=?1 AND agent_role=?2 AND stage='Specification'",params![request.project_id,request.agent_role]).map_err(db_error)?;
        }
        connection.commit().map_err(db_error)?;
        self.list_bindings()?
            .into_iter()
            .find(|item| {
                item.project_id == request.project_id
                    && item.agent_role == request.agent_role
                    && item.stage == request.stage
            })
            .ok_or_else(|| {
                LlmError::new(
                    LlmErrorKind::Persistence,
                    "The saved binding was not found.",
                )
            })
    }

    pub fn list_free_form_bindings(
        &self,
        project_id: &str,
    ) -> Result<Vec<FreeFormProviderRouteBinding>, LlmError> {
        let project_id = non_empty(project_id, "projectId")?;
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT project_id,slot_id,route_id,enabled,version,created_at,updated_at
                 FROM free_form_provider_route_bindings
                 WHERE project_id=?1 ORDER BY slot_id",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![project_id], |row| {
                Ok(FreeFormProviderRouteBinding {
                    project_id: row.get(0)?,
                    slot_id: row.get(1)?,
                    route_id: row.get(2)?,
                    enabled: row.get(3)?,
                    version: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn upsert_free_form_binding(
        &self,
        request: UpsertFreeFormProviderRouteBindingRequest,
    ) -> Result<FreeFormProviderRouteBinding, LlmError> {
        let project_id = non_empty(&request.project_id, "projectId")?;
        let slot_id = non_empty(&request.slot_id, "slotId")?;
        let route_id = non_empty(&request.route_id, "routeId")?;
        let mut db = self.connection()?;
        let connection = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let valid_slot: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_slots
                 WHERE id=?1 AND project_id=?2 AND kind='free_form')",
                params![slot_id, project_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !valid_slot {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "FreeForm slot does not belong to this project.",
            ));
        }
        let valid_route: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM provider_routes WHERE id=?1)",
                params![route_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !valid_route {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "FreeForm binding references an unknown route.",
            ));
        }
        let actual = connection
            .query_row(
                "SELECT version FROM free_form_provider_route_bindings
                 WHERE project_id=?1 AND slot_id=?2",
                params![project_id, slot_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()
            .map_err(db_error)?;
        let now = Utc::now().to_rfc3339();
        if let Some(actual) = actual {
            let expected = request.version.ok_or_else(|| version_conflict(0, actual))?;
            if expected != actual {
                return Err(version_conflict(expected, actual));
            }
            connection
                .execute(
                    "UPDATE free_form_provider_route_bindings
                     SET route_id=?1,enabled=?2,version=version+1,updated_at=?3
                     WHERE project_id=?4 AND slot_id=?5 AND version=?6",
                    params![
                        route_id,
                        request.enabled.unwrap_or(true),
                        now,
                        project_id,
                        slot_id,
                        expected
                    ],
                )
                .map_err(db_error)?;
        } else {
            connection
                .execute(
                    "INSERT INTO free_form_provider_route_bindings
                     (project_id,slot_id,route_id,enabled,version,created_at,updated_at)
                     VALUES (?1,?2,?3,?4,1,?5,?5)",
                    params![
                        project_id,
                        slot_id,
                        route_id,
                        request.enabled.unwrap_or(true),
                        now
                    ],
                )
                .map_err(db_error)?;
        }
        connection.commit().map_err(db_error)?;
        self.list_free_form_bindings(project_id)?
            .into_iter()
            .find(|binding| binding.slot_id == slot_id)
            .ok_or_else(|| {
                LlmError::new(
                    LlmErrorKind::Persistence,
                    "Saved FreeForm binding was not found.",
                )
            })
    }

    fn find_raw_connection(&self, id: &str) -> Result<Option<(u64, String, String)>, LlmError> {
        self.connection()?
            .query_row(
                "SELECT version,provider_id,credential_ref FROM provider_connections WHERE id=?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(db_error)
    }

    pub fn find_resolved_connection(&self, id: &str) -> Result<ResolvedConnection, LlmError> {
        self.connection()?.query_row(
            "SELECT id,provider_id,credential_ref,endpoint_url,default_model,enabled,state,cooldown_until,backoff_level,max_concurrency
             FROM provider_connections WHERE id=?1",params![id],|row|{
                Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,String>(4)?,row.get::<_,bool>(5)?,row.get::<_,String>(6)?,row.get::<_,Option<String>>(7)?,row.get::<_,u32>(8)?,row.get::<_,u32>(9)?))
            }
        ).optional().map_err(db_error)?.map(|(id,provider_id,credential_ref,endpoint_url,default_model,enabled,state,cooldown_until,backoff_level,max_concurrency)|{
            let profile=profile_by_id(&provider_id).ok_or_else(||LlmError::new(LlmErrorKind::InvalidRequest,"The connection provider profile does not exist."))?;
            let endpoint=profile.resolved_endpoint(endpoint_url.as_deref())?;
            Ok(ResolvedConnection{id:id.into(),provider_id:provider_id.into(),credential_ref,endpoint_url:endpoint,default_model,enabled,state,cooldown_until,backoff_level,max_concurrency})
        }).transpose()?.ok_or_else(||LlmError::new(LlmErrorKind::InvalidRequest,"Connection not found."))
    }

    pub fn create_execution(
        &self,
        execution_id: &str,
        generation_id: &str,
        project_id: &str,
        pulsation_id: Option<&str>,
        job_id: Option<&str>,
        route: &ProviderRoute,
        target: &AgentExecutionTarget,
        stage: &str,
        mode: &str,
        decision: &RouteDecision,
    ) -> Result<(), LlmError> {
        self.connection()?.execute(
            "INSERT INTO llm_route_executions (id,generation_id,project_id,pulsation_id,job_id,route_id,agent_role,free_form_slot_id,stage,strategy,mode,status,decision_json,started_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'running',?12,?13)",
            params![execution_id,generation_id,project_id,pulsation_id,job_id,route.id,target.agent_role().map(AgentRole::as_str),target.free_form_slot_id(),stage,route.strategy,mode,serde_json::to_string(decision).unwrap_or_else(|_|"{}".to_string()),Utc::now().to_rfc3339()]
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn start_attempt(
        &self,
        execution_id: &str,
        ordinal: u32,
        target: &ResolvedRouteTarget,
        model: &str,
    ) -> Result<String, LlmError> {
        let id = Uuid::new_v4().to_string();
        self.connection()?.execute(
            "INSERT INTO llm_attempts (id,execution_id,ordinal,connection_id,provider_id,model_id,outcome,started_at)
             VALUES (?1,?2,?3,?4,?5,?6,'running',?7)",
            params![id,execution_id,ordinal,target.connection.id.as_str(),target.connection.provider_id.as_str(),model,Utc::now().to_rfc3339()]
        ).map_err(db_error)?;
        Ok(id)
    }

    pub fn finish_attempt(
        &self,
        id: &str,
        outcome: &str,
        error_kind: Option<&str>,
        http_status: Option<u16>,
        provider_code: Option<&str>,
        latency_ms: u64,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        cost: Option<f64>,
    ) -> Result<(), LlmError> {
        let provider_code = provider_code
            .map(super::adapter::sanitize_model_text)
            .transpose()?;
        self.connection()?.execute(
            "UPDATE llm_attempts SET outcome=?1,error_kind=?2,http_status=?3,provider_code=?4,latency_ms=?5,input_tokens=?6,output_tokens=?7,estimated_cost_usd=?8,finished_at=?9 WHERE id=?10",
            params![outcome,error_kind,http_status,provider_code.as_deref(),latency_ms,input_tokens,output_tokens,cost,Utc::now().to_rfc3339(),id]
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn record_model_observation(
        &self,
        attempt_id: &str,
        ordinal: u32,
        generation_id: &str,
        agent_role: Option<AgentRole>,
        agent_target: &str,
        parent_invocation_id: Option<&str>,
        provider_id: &str,
        requested_model: &str,
        observed_model: Option<&str>,
    ) -> Result<(), LlmError> {
        self.connection()?
            .execute(
                "INSERT INTO llm_model_observations(
               attempt_id,ordinal,generation_id,agent_role,agent_target,parent_invocation_id,
               provider_id,requested_model,observed_model,created_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    attempt_id,
                    ordinal,
                    generation_id,
                    agent_role.map(AgentRole::as_str),
                    agent_target,
                    parent_invocation_id,
                    provider_id,
                    requested_model,
                    observed_model,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(db_error)?;
        if let Some(model) = observed_model {
            self.connection()?
                .execute(
                    "UPDATE llm_attempts SET observed_model=?1 WHERE id=?2",
                    params![model, attempt_id],
                )
                .map_err(db_error)?;
        }
        Ok(())
    }

    pub fn finish_execution(
        &self,
        id: &str,
        status: &str,
        next_eligible_at: Option<&str>,
        visible_output: Option<&str>,
        complete: bool,
    ) -> Result<(), LlmError> {
        let visible_output = visible_output
            .map(super::adapter::sanitize_model_text)
            .transpose()?;
        self.connection()?.execute(
            "UPDATE llm_route_executions SET status=?1,next_eligible_at=?2,visible_output=CASE WHEN ?4=1 THEN ?3 ELSE NULL END,visible_output_complete=?4,finished_at=?5 WHERE id=?6",
            params![status,next_eligible_at,if complete { visible_output.as_deref() } else { None },complete,Utc::now().to_rfc3339(),id]
        ).map_err(db_error)?;
        Ok(())
    }

    /// One final logical output per execution. Repeated writes replace the
    /// same evidence rather than multiplying counters across retries/views.
    pub fn record_output_redaction(
        &self,
        id: &str,
        count: u64,
        categories: &[String],
    ) -> Result<(), LlmError> {
        let categories = categories
            .iter()
            .map(|category| super::adapter::sanitize_model_text(category))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        let categories =
            serde_json::to_string(&categories).map_err(|_| super::adapter::sanitization_error())?;
        let changed = self.connection()?.execute(
            "UPDATE llm_route_executions SET output_redaction_count=?2,output_redaction_categories_json=?3 WHERE id=?1 AND status='completed' AND visible_output_complete=1",
            params![id,count.min(i64::MAX as u64) as i64,categories],
        ).map_err(db_error)?;
        if changed != 1 {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "Output sanitization evidence requires a complete execution.",
            ));
        }
        Ok(())
    }

    /// Compatibility entry point: incomplete fragments cannot be qualified
    /// independently, so they are deliberately withheld from durable history.
    pub fn update_visible_output(&self, id: &str, _visible_output: &str) -> Result<(), LlmError> {
        self.connection()?.execute(
            "UPDATE llm_route_executions SET visible_output=NULL,visible_output_complete=0 WHERE id=?1",
            params![id],
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn claim_breaker_probe(
        &self,
        provider_id: &str,
        endpoint: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, LlmError> {
        let connection = self.connection()?;
        let origin = endpoint_origin(endpoint);
        let state = connection.query_row(
            "SELECT state,probe_after,probe_in_flight FROM provider_circuit_breakers WHERE provider_id=?1 AND endpoint_origin=?2",
            params![provider_id, origin],
            |row| Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,bool>(2)?)),
        ).optional().map_err(db_error)?;
        let Some((state, probe_after, probe_in_flight)) = state else {
            return Ok(true);
        };
        if matches!(state.as_str(), "closed" | "degraded") {
            return Ok(true);
        }
        if probe_in_flight {
            return Ok(false);
        }
        let probe_ready = probe_after
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_none_or(|value| value.with_timezone(&Utc) <= now);
        if !probe_ready {
            return Ok(false);
        }
        let changed = connection.execute(
            "UPDATE provider_circuit_breakers SET state='half_open',probe_in_flight=1,version=version+1,updated_at=?1
             WHERE provider_id=?2 AND endpoint_origin=?3 AND state IN ('open','half_open') AND probe_in_flight=0",
            params![now.to_rfc3339(),provider_id,origin],
        ).map_err(db_error)?;
        Ok(changed == 1)
    }

    pub fn set_affinity(
        &self,
        affinity_hash: &str,
        route_id: &str,
        connection_id: &str,
        model_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), LlmError> {
        self.connection()?.execute(
            "INSERT INTO llm_route_affinity (affinity_hash,route_id,connection_id,model_id,last_success_at,expires_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(affinity_hash,route_id) DO UPDATE SET connection_id=excluded.connection_id,model_id=excluded.model_id,last_success_at=excluded.last_success_at,expires_at=excluded.expires_at",
            params![affinity_hash,route_id,connection_id,model_id,now.to_rfc3339(),(now+ChronoDuration::minutes(30)).to_rfc3339()]
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn mark_connection_blocked(
        &self,
        connection_id: &str,
        reason: &str,
    ) -> Result<(), LlmError> {
        self.connection()?.execute("UPDATE provider_connections SET state='blocked',blocked_reason=?1,updated_at=?2 WHERE id=?3",params![reason,Utc::now().to_rfc3339(),connection_id]).map_err(db_error)?;
        Ok(())
    }

    pub fn apply_cooldown(&self, connection_id: &str, until: &str) -> Result<(), LlmError> {
        self.connection()?.execute("UPDATE provider_connections SET cooldown_until=?1,backoff_level=backoff_level+1,updated_at=?2 WHERE id=?3",params![until,Utc::now().to_rfc3339(),connection_id]).map_err(db_error)?;
        Ok(())
    }

    pub fn clear_connection_penalties(&self, connection_id: &str) -> Result<(), LlmError> {
        self.connection()?.execute("UPDATE provider_connections SET cooldown_until=NULL,backoff_level=0,updated_at=?1 WHERE id=?2",params![Utc::now().to_rfc3339(),connection_id]).map_err(db_error)?;
        Ok(())
    }

    pub fn lockout_model(
        &self,
        connection_id: &str,
        model: &str,
        reason: &str,
    ) -> Result<(), LlmError> {
        let now = Utc::now().to_rfc3339();
        self.connection()?.execute(
            "INSERT INTO provider_model_lockouts(connection_id,model_id,reason,locked_until,created_at,updated_at)
             VALUES (?1,?2,?3,NULL,?4,?4) ON CONFLICT(connection_id,model_id) DO UPDATE SET reason=excluded.reason,locked_until=NULL,updated_at=excluded.updated_at",
            params![connection_id,model,reason,now]
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn record_health_failure(
        &self,
        provider_id: &str,
        endpoint_origin: &str,
        local: bool,
        now: DateTime<Utc>,
    ) -> Result<(), LlmError> {
        let connection = self.connection()?;
        let existing=connection.query_row(
            "SELECT state,failure_count,window_started_at FROM provider_circuit_breakers WHERE provider_id=?1 AND endpoint_origin=?2",
            params![provider_id,endpoint_origin],|row|Ok((row.get::<_,String>(0)?,row.get::<_,u32>(1)?,row.get::<_,Option<String>>(2)?))
        ).optional().map_err(db_error)?;
        let window_seconds = if local { 30 } else { 60 };
        let degraded_at = if local { 1 } else { 3 };
        let open_at = if local { 2 } else { 5 };
        let probe_seconds = if local { 15 } else { 30 };
        let mut failures = existing.as_ref().map(|item| item.1).unwrap_or(0);
        let previous_window_started = existing
            .as_ref()
            .and_then(|item| item.2.as_deref())
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));
        let window_expired = previous_window_started
            .is_none_or(|start| now.signed_duration_since(start).num_seconds() > window_seconds);
        if window_expired {
            failures = 0;
        }
        let window_started = if window_expired {
            now
        } else {
            previous_window_started.unwrap_or(now)
        };
        failures += 1;
        let state = if failures >= open_at {
            "open"
        } else if failures >= degraded_at {
            "degraded"
        } else {
            "closed"
        };
        let probe_after =
            (state == "open").then(|| (now + ChronoDuration::seconds(probe_seconds)).to_rfc3339());
        connection.execute(
            "INSERT INTO provider_circuit_breakers(provider_id,endpoint_origin,state,failure_count,window_started_at,opened_at,probe_after,probe_in_flight,version,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,0,1,?8)
             ON CONFLICT(provider_id,endpoint_origin) DO UPDATE SET state=excluded.state,failure_count=excluded.failure_count,window_started_at=excluded.window_started_at,opened_at=excluded.opened_at,probe_after=excluded.probe_after,probe_in_flight=0,version=provider_circuit_breakers.version+1,updated_at=excluded.updated_at",
            params![provider_id,endpoint_origin,state,failures,window_started.to_rfc3339(),(state=="open").then(||now.to_rfc3339()),probe_after,now.to_rfc3339()]
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn record_health_success(
        &self,
        provider_id: &str,
        endpoint_origin: &str,
        now: DateTime<Utc>,
    ) -> Result<(), LlmError> {
        self.connection()?.execute(
            "INSERT INTO provider_circuit_breakers(provider_id,endpoint_origin,state,failure_count,probe_in_flight,version,updated_at)
             VALUES (?1,?2,'closed',0,0,1,?3) ON CONFLICT(provider_id,endpoint_origin) DO UPDATE SET state='closed',failure_count=0,window_started_at=NULL,opened_at=NULL,probe_after=NULL,probe_in_flight=0,version=provider_circuit_breakers.version+1,updated_at=excluded.updated_at",
            params![provider_id,endpoint_origin,now.to_rfc3339()]
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn breaker_state(
        &self,
        provider_id: &str,
        endpoint_origin: &str,
    ) -> Result<Option<String>, LlmError> {
        self.connection()?
            .query_row(
                "SELECT state FROM provider_circuit_breakers WHERE provider_id=?1 AND endpoint_origin=?2",
                params![provider_id, endpoint_origin],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)
    }

    pub fn next_eligible_at(
        &self,
        targets: &[ResolvedRouteTarget],
        now: DateTime<Utc>,
    ) -> Result<Option<String>, LlmError> {
        let connection = self.connection()?;
        let mut earliest: Option<DateTime<Utc>> = None;
        for target in targets {
            let model = if target.target.model_id.trim().is_empty() {
                target.connection.default_model.as_str()
            } else {
                target.target.model_id.as_str()
            };
            let breaker_probe = connection
                .query_row(
                    "SELECT probe_after FROM provider_circuit_breakers WHERE provider_id=?1 AND endpoint_origin=?2 AND state IN ('open','half_open')",
                    params![target.connection.provider_id.as_str(), endpoint_origin(&target.connection.endpoint_url)],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(db_error)?
                .flatten();
            let lockout_until = connection
                .query_row(
                    "SELECT locked_until FROM provider_model_lockouts WHERE connection_id=?1 AND model_id=?2",
                    params![target.connection.id.as_str(), model],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(db_error)?
                .flatten();
            for candidate in [
                target.connection.cooldown_until.as_deref(),
                breaker_probe.as_deref(),
                lockout_until.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                let Some(at) = DateTime::parse_from_rfc3339(candidate)
                    .ok()
                    .map(|value| value.with_timezone(&Utc))
                    .filter(|value| *value > now)
                else {
                    continue;
                };
                if earliest.is_none_or(|current| at < current) {
                    earliest = Some(at);
                }
            }
        }
        Ok(earliest.map(|value| value.to_rfc3339()))
    }

    pub fn resilience_snapshot(&self) -> Result<LlmResilienceSnapshot, LlmError> {
        let connection = self.connection()?;
        let mut states = Vec::new();
        let mut statement=connection.prepare("SELECT id,state,cooldown_until,blocked_reason FROM provider_connections WHERE state!='active' OR cooldown_until IS NOT NULL ORDER BY id").map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(ResilienceStateItem {
                    scope: "connection".to_string(),
                    scope_id: row.get(0)?,
                    state: row.get(1)?,
                    retry_at: row.get(2)?,
                    reason: row.get(3)?,
                })
            })
            .map_err(db_error)?;
        for row in rows {
            states.push(row.map_err(db_error)?);
        }
        let mut statement=connection.prepare("SELECT provider_id||'@'||endpoint_origin,state,probe_after FROM provider_circuit_breakers WHERE state!='closed' ORDER BY provider_id,endpoint_origin").map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(ResilienceStateItem {
                    scope: "breaker".to_string(),
                    scope_id: row.get(0)?,
                    state: row.get(1)?,
                    retry_at: row.get(2)?,
                    reason: None,
                })
            })
            .map_err(db_error)?;
        for row in rows {
            states.push(row.map_err(db_error)?);
        }
        let mut statement=connection.prepare("SELECT connection_id||'@'||model_id,locked_until,reason FROM provider_model_lockouts ORDER BY connection_id,model_id").map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(ResilienceStateItem {
                    scope: "model".to_string(),
                    scope_id: row.get(0)?,
                    state: "locked".to_string(),
                    retry_at: row.get(1)?,
                    reason: row.get(2)?,
                })
            })
            .map_err(db_error)?;
        for row in rows {
            states.push(row.map_err(db_error)?);
        }
        Ok(LlmResilienceSnapshot {
            generated_at: Utc::now().to_rfc3339(),
            states,
        })
    }

    pub fn reset_resilience(
        &self,
        request: ResetLlmResilienceStateRequest,
    ) -> Result<usize, LlmError> {
        let connection = self.connection()?;
        let mut changed = 0;
        if let Some(id) = request.connection_id {
            changed+=connection.execute("UPDATE provider_connections SET state='active',blocked_reason=NULL,cooldown_until=NULL,backoff_level=0,updated_at=?1 WHERE id=?2",params![Utc::now().to_rfc3339(),id]).map_err(db_error)?;
            changed += connection
                .execute(
                    "DELETE FROM provider_model_lockouts WHERE connection_id=?1",
                    params![id],
                )
                .map_err(db_error)?;
        }
        if let Some(provider) = request.provider_id {
            changed += connection
                .execute(
                    "DELETE FROM provider_circuit_breakers WHERE provider_id=?1",
                    params![provider],
                )
                .map_err(db_error)?;
        }
        Ok(changed)
    }

    pub fn execution_snapshot(&self, id: &str) -> Result<LlmExecutionSnapshot, LlmError> {
        self.execution_snapshot_with_evidence(id)
            .map(|(view, _)| view)
    }

    /// Hash the complete selected evidence before producing its observable view.
    /// This hash never identifies an isolated matched value.
    pub(crate) fn execution_snapshot_with_evidence(
        &self,
        id: &str,
    ) -> Result<(LlmExecutionSnapshot, String), LlmError> {
        let connection = self.connection()?;
        let base=connection.query_row(
            "SELECT generation_id,route_id,status,decision_json,next_eligible_at,visible_output,visible_output_complete FROM llm_route_executions WHERE id=?1",params![id],
            |row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,bool>(6)?))
        ).optional().map_err(db_error)?.ok_or_else(||LlmError::new(LlmErrorKind::InvalidRequest,"LLM execution not found."))?;
        let mut statement=connection.prepare("SELECT ordinal,connection_id,provider_id,model_id,outcome,error_kind,http_status,latency_ms,input_tokens,output_tokens,estimated_cost_usd,started_at,finished_at FROM llm_attempts WHERE execution_id=?1 ORDER BY ordinal").map_err(db_error)?;
        let rows=statement.query_map(params![id],|row|Ok(json!({"ordinal":row.get::<_,u32>(0)?,"connectionId":row.get::<_,String>(1)?,"providerId":row.get::<_,String>(2)?,"modelId":row.get::<_,String>(3)?,"outcome":row.get::<_,String>(4)?,"errorKind":row.get::<_,Option<String>>(5)?,"httpStatus":row.get::<_,Option<u16>>(6)?,"latencyMs":row.get::<_,Option<u64>>(7)?,"inputTokens":row.get::<_,Option<i64>>(8)?,"outputTokens":row.get::<_,Option<i64>>(9)?,"estimatedCostUsd":row.get::<_,Option<f64>>(10)?,"startedAt":row.get::<_,String>(11)?,"finishedAt":row.get::<_,Option<String>>(12)?}))).map_err(db_error)?;
        let mut attempts = rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?;
        let evidence = json!({
            "executionId": id,
            "generationId": base.0,
            "routeId": base.1,
            "status": base.2,
            "decisionJson": base.3,
            "nextEligibleAt": base.4,
            "visibleOutput": base.5,
            "visibleOutputComplete": base.6,
            "attempts": attempts,
        });
        let evidence = canonical_evidence_json(&evidence)?;
        let original_evidence_sha256 = crate::secret_redactor::text_sha256(&evidence);
        for attempt in &mut attempts {
            super::adapter::sanitize_json_strings(attempt)?;
        }
        let mut decision = serde_json::from_str(&base.3).unwrap_or(Value::Null);
        super::adapter::sanitize_json_strings(&mut decision)?;
        Ok((
            LlmExecutionSnapshot {
                execution_id: id.to_string(),
                generation_id: base.0,
                route_id: base.1,
                status: base.2,
                decision,
                next_eligible_at: base.4,
                visible_output: if base.6 {
                    base.5
                        .map(|text| super::adapter::sanitize_model_text(&text))
                        .transpose()?
                } else {
                    None
                },
                visible_output_complete: base.6,
                attempts,
            },
            original_evidence_sha256,
        ))
    }

    pub fn routing_metrics(&self) -> Result<Vec<LlmRoutingMetric>, LlmError> {
        let since = (Utc::now() - ChronoDuration::days(7)).to_rfc3339();
        let connection = self.connection()?;
        let mut statement=connection.prepare(
            "SELECT provider_id,model_id,COUNT(*),SUM(CASE WHEN outcome='success' THEN 1 ELSE 0 END),AVG(latency_ms),COALESCE(SUM(input_tokens),0),COALESCE(SUM(output_tokens),0),SUM(estimated_cost_usd)
             FROM llm_attempts WHERE started_at>=?1 GROUP BY provider_id,model_id ORDER BY provider_id,model_id"
        ).map_err(db_error)?;
        let rows = statement
            .query_map(params![since], |row| {
                Ok(LlmRoutingMetric {
                    provider_id: row.get(0)?,
                    model_id: row.get(1)?,
                    attempts: row.get(2)?,
                    successes: row.get(3)?,
                    average_latency_ms: row.get(4)?,
                    input_tokens: row.get(5)?,
                    output_tokens: row.get(6)?,
                    estimated_cost_usd: row.get(7)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn record_compression_metric(
        &self,
        execution_id: Option<&str>,
        preview: &CompressionPreview,
    ) -> Result<(), LlmError> {
        self.connection()?
            .execute(
                "INSERT INTO llm_compression_metrics (
               id,execution_id,engine,engine_version,filter_name,content_kind,mode,
               original_sha256,original_bytes,compressed_bytes,duration_ms,result,created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,'preview',?7,?8,?9,?10,?11,?12)",
                params![
                    Uuid::new_v4().to_string(),
                    execution_id,
                    preview.engine,
                    preview.engine_version,
                    preview.filter_name,
                    format!("{:?}", preview.content_kind).to_ascii_lowercase(),
                    preview.original_sha256,
                    preview.original_bytes as i64,
                    preview.compressed_bytes as i64,
                    preview.duration_ms as i64,
                    preview.result,
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn resolve_route_for_target(
        &self,
        project_id: &str,
        target: &AgentExecutionTarget,
        stage: &str,
    ) -> Result<ProviderRoute, LlmError> {
        match target {
            AgentExecutionTarget::Canonical { agent_role } => {
                <Self as LlmRepository>::resolve_route(self, project_id, *agent_role, stage)
            }
            AgentExecutionTarget::Ephemeral { agent_role, .. } => {
                <Self as LlmRepository>::resolve_route(self, project_id, *agent_role, stage)
            }
            AgentExecutionTarget::FreeForm { slot_id } => {
                let connection = self.connection()?;
                let valid_slot: bool = connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM terminal_slots
                         WHERE id=?1 AND project_id=?2 AND kind='free_form' AND archived=0)",
                        params![slot_id, project_id],
                        |row| row.get(0),
                    )
                    .map_err(db_error)?;
                if !valid_slot {
                    return Err(LlmError::new(
                        LlmErrorKind::InvalidRequest,
                        "FreeForm slot does not belong to this project or is archived.",
                    ));
                }
                let route_id = connection
                    .query_row(
                        "SELECT route_id FROM free_form_provider_route_bindings
                         WHERE project_id=?1 AND slot_id=?2 AND enabled=1",
                        params![project_id, slot_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(db_error)?
                    .ok_or_else(|| {
                        LlmError::new(
                            LlmErrorKind::InvalidRequest,
                            "Configure an API route for this FreeForm agent before sending.",
                        )
                    })?;
                self.list_routes()?
                    .into_iter()
                    .find(|route| route.id == route_id && route.enabled)
                    .ok_or_else(|| {
                        LlmError::new(
                            LlmErrorKind::InvalidRequest,
                            "The FreeForm agent route is missing or disabled.",
                        )
                    })
            }
        }
    }

    pub fn list_executions_for_target(
        &self,
        project_id: &str,
        target: &AgentExecutionTarget,
        limit: usize,
    ) -> Result<Vec<LlmExecutionListItem>, LlmError> {
        let project_id = non_empty(project_id, "projectId")?;
        let limit = limit.clamp(1, 50) as i64;
        let connection = self.connection()?;
        let (predicate, target_value) = match target {
            AgentExecutionTarget::Canonical { agent_role } => (
                "agent_role=?2 AND free_form_slot_id IS NULL",
                agent_role.as_str(),
            ),
            AgentExecutionTarget::Ephemeral { .. } => {
                return Ok(Vec::new());
            }
            AgentExecutionTarget::FreeForm { slot_id } => (
                "agent_role IS NULL AND free_form_slot_id=?2",
                slot_id.as_str(),
            ),
        };
        let sql = format!(
            "SELECT id,generation_id,agent_role,free_form_slot_id,route_id,stage,status,
                    visible_output,visible_output_complete,started_at,finished_at
             FROM llm_route_executions
             WHERE project_id=?1 AND {predicate}
             ORDER BY started_at DESC LIMIT ?3"
        );
        let mut statement = connection.prepare(&sql).map_err(db_error)?;
        let rows = statement
            .query_map(params![project_id, target_value, limit], |row| {
                let role: Option<String> = row.get(2)?;
                let slot_id: Option<String> = row.get(3)?;
                let target = match (role.as_deref().and_then(AgentRole::parse), slot_id) {
                    (Some(agent_role), None) => AgentExecutionTarget::canonical(agent_role),
                    (None, Some(slot_id)) => AgentExecutionTarget::free_form(slot_id),
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                Ok(LlmExecutionListItem {
                    execution_id: row.get(0)?,
                    generation_id: row.get(1)?,
                    target,
                    route_id: row.get(4)?,
                    stage: row.get(5)?,
                    status: row.get(6)?,
                    visible_output: row.get(7)?,
                    visible_output_complete: row.get(8)?,
                    started_at: row.get(9)?,
                    finished_at: row.get(10)?,
                })
            })
            .map_err(db_error)?;
        let mut items = rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?;
        for item in &mut items {
            item.visible_output = if item.visible_output_complete {
                item.visible_output
                    .take()
                    .map(|text| super::adapter::sanitize_model_text(&text))
                    .transpose()?
            } else {
                None
            };
        }
        Ok(items)
    }

    pub fn execution_belongs_to_target(
        &self,
        project_id: &str,
        target: &AgentExecutionTarget,
        execution_id: &str,
    ) -> Result<bool, LlmError> {
        let project_id = non_empty(project_id, "projectId")?;
        let execution_id = non_empty(execution_id, "executionId")?;
        let connection = self.connection()?;
        let found = match target {
            AgentExecutionTarget::Canonical { agent_role } => connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM llm_route_executions
                 WHERE id=?1 AND project_id=?2 AND agent_role=?3
                   AND free_form_slot_id IS NULL)",
                params![execution_id, project_id, agent_role.as_str()],
                |row| row.get(0),
            ),
            AgentExecutionTarget::Ephemeral { .. } => return Ok(false),
            AgentExecutionTarget::FreeForm { slot_id } => connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM llm_route_executions
                 WHERE id=?1 AND project_id=?2 AND agent_role IS NULL
                   AND free_form_slot_id=?3)",
                params![execution_id, project_id, slot_id],
                |row| row.get(0),
            ),
        }
        .map_err(db_error)?;
        Ok(found)
    }
}

impl LlmRepository for SqliteLlmRepository {
    fn resolve_route(
        &self,
        project_id: &str,
        role: AgentRole,
        stage: &str,
    ) -> Result<ProviderRoute, LlmError> {
        let connection = self.connection()?;
        let stage = crate::orchestration::catalog::canonical_stage(stage);
        if stage == "SPECIFICATION" {
            let conflict: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM provider_route_bindings a JOIN provider_route_bindings c ON c.project_id=a.project_id AND c.agent_role=a.agent_role WHERE a.project_id=?1 AND a.agent_role=?2 AND a.stage='Specification' AND c.stage='SPECIFICATION' AND (a.route_id<>c.route_id OR a.enabled<>c.enabled))",params![project_id,role.as_str()],|r|r.get(0)).map_err(db_error)?;
            if conflict {
                return Err(LlmError::new(
                    LlmErrorKind::RouteConflict,
                    "Conflicting Specification/SPECIFICATION bindings. Fix the bindings in Settings.",
                ));
            }
        }
        let route_id=connection.query_row(
            "SELECT route_id FROM provider_route_bindings WHERE project_id=?1 AND agent_role=?2 AND stage=?3 AND enabled=1",
            params![project_id,role.as_str(),stage],|row|row.get::<_,String>(0)
        ).optional().map_err(db_error)?.unwrap_or_else(||"legacy-default".to_string());
        self.list_routes()?
            .into_iter()
            .find(|route| route.id == route_id && route.enabled)
            .ok_or_else(|| LlmError::new(LlmErrorKind::InvalidRequest, "Active route not found."))
    }

    fn resolved_targets(&self, route_id: &str) -> Result<Vec<ResolvedRouteTarget>, LlmError> {
        let connection = self.connection()?;
        let targets = self.targets_for_route(&connection, route_id)?;
        let mut result = Vec::new();
        for target in targets.into_iter().filter(|target| target.enabled) {
            let resolved = self.find_resolved_connection(&target.connection_id)?;
            let local =
                profile_by_id(resolved.provider_id.as_str()).is_some_and(|profile| profile.local);
            result.push(ResolvedRouteTarget {
                target,
                connection: resolved,
                provider_local: local,
            });
        }
        Ok(result)
    }

    fn decide(
        &self,
        route: &ProviderRoute,
        targets: &[ResolvedRouteTarget],
        affinity_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<RouteDecision, LlmError> {
        let connection = self.connection()?;
        let affinity=connection.query_row(
            "SELECT connection_id,model_id FROM llm_route_affinity WHERE affinity_hash=?1 AND route_id=?2 AND expires_at>?3",
            params![affinity_hash,route.id,now.to_rfc3339()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))
        ).optional().map_err(db_error)?;
        let metrics = self.routing_metrics()?;
        let latency_values = targets
            .iter()
            .map(|target| {
                let model = if target.target.model_id.trim().is_empty() {
                    target.connection.default_model.as_str()
                } else {
                    target.target.model_id.as_str()
                };
                metrics
                    .iter()
                    .find(|item| {
                        item.provider_id == target.connection.provider_id.as_str()
                            && item.model_id == model
                    })
                    .and_then(|item| item.average_latency_ms)
            })
            .collect::<Vec<_>>();
        let cost_values = targets
            .iter()
            .map(|target| {
                let model = if target.target.model_id.trim().is_empty() {
                    target.connection.default_model.as_str()
                } else {
                    target.target.model_id.as_str()
                };
                model_price_basis(target.connection.provider_id.as_str(), model)
            })
            .collect::<Vec<_>>();
        let latency_inverse = normalize_metric(&latency_values, true);
        let cost_inverse = normalize_metric(&cost_values, true);
        let mut candidates = Vec::new();
        let mut eligible_targets = Vec::new();
        for (target_index, target) in targets.iter().enumerate() {
            let model = if target.target.model_id.trim().is_empty() {
                target.connection.default_model.clone()
            } else {
                target.target.model_id.clone()
            };
            let mut gate = None;
            if !target.connection.enabled {
                gate = Some("connection_disabled".to_string());
            } else if target.connection.state != "active" {
                gate = Some("connection_blocked".to_string());
            } else if target
                .connection
                .cooldown_until
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|until| until.with_timezone(&Utc) > now)
            {
                gate = Some("connection_cooldown".to_string());
            } else {
                let locked:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM provider_model_lockouts WHERE connection_id=?1 AND model_id=?2 AND (locked_until IS NULL OR locked_until>?3))",params![target.connection.id.as_str(),model,now.to_rfc3339()],|row|row.get(0)).map_err(db_error)?;
                if locked {
                    gate = Some("model_lockout".to_string());
                }
                let breaker:Option<(String,Option<String>,bool)>=connection.query_row("SELECT state,probe_after,probe_in_flight FROM provider_circuit_breakers WHERE provider_id=?1 AND endpoint_origin=?2",params![target.connection.provider_id.as_str(),endpoint_origin(&target.connection.endpoint_url)],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(db_error)?;
                if let Some((state, probe_after, probe_in_flight)) = breaker {
                    if state == "open"
                        && probe_after
                            .as_deref()
                            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                            .is_some_and(|probe| probe.with_timezone(&Utc) > now)
                    {
                        gate = Some("breaker_open".to_string());
                    } else if (state == "open" || state == "half_open") && probe_in_flight {
                        gate = Some("breaker_probe_busy".to_string());
                    }
                }
            }
            let metric = metrics.iter().find(|item| {
                item.provider_id == target.connection.provider_id.as_str() && item.model_id == model
            });
            let sample = metric.map(|item| item.attempts).unwrap_or(0);
            let reliability = metric.map(|item| {
                if item.attempts == 0 {
                    0.5
                } else {
                    item.successes as f64 / item.attempts as f64
                }
            });
            let breaker_health = connection.query_row(
                "SELECT state FROM provider_circuit_breakers WHERE provider_id=?1 AND endpoint_origin=?2",
                params![target.connection.provider_id.as_str(), endpoint_origin(&target.connection.endpoint_url)],
                |row| row.get::<_, String>(0),
            ).optional().map_err(db_error)?.map_or(1.0, |state| match state.as_str() {
                "degraded" => 0.5,
                "half_open" => 0.25,
                "open" => 0.0,
                _ => 1.0,
            });
            let breakdown = score(&ScoreInput {
                health: if gate.is_none() { breaker_health } else { 0.0 },
                latency_inverse: Some(latency_inverse[target_index]),
                reliability,
                cost_inverse: Some(cost_inverse[target_index]),
                task_fit: None,
                connection_density: 1.0,
                sample_size: sample,
            });
            candidates.push(CandidateDecision {
                position: target.target.position,
                connection_id: target.connection.id.0.clone(),
                provider_id: target.connection.provider_id.0.clone(),
                model_id: model.clone(),
                eligible: gate.is_none(),
                gate_reason: gate.clone(),
                score: (route.strategy == "score").then_some(breakdown.total),
                score_factors: (route.strategy == "score")
                    .then(|| serde_json::to_value(&breakdown).unwrap_or(Value::Null)),
            });
            if gate.is_none() {
                eligible_targets.push(target.target.clone());
            }
        }
        if route.strategy == "lkgp" {
            if let Some((connection_id, model_id)) = affinity
                && let Some(index) = eligible_targets.iter().position(|target| {
                    target.connection_id == connection_id && target.model_id == model_id
                })
            {
                let target = eligible_targets.remove(index);
                eligible_targets.insert(0, target);
            }
        } else if route.strategy == "score" {
            eligible_targets.sort_by(|a, b| {
                let score_a = candidates
                    .iter()
                    .find(|item| {
                        item.connection_id == a.connection_id && item.model_id == a.model_id
                    })
                    .and_then(|item| item.score)
                    .unwrap_or(0.0);
                let score_b = candidates
                    .iter()
                    .find(|item| {
                        item.connection_id == b.connection_id && item.model_id == b.model_id
                    })
                    .and_then(|item| item.score)
                    .unwrap_or(0.0);
                score_b
                    .partial_cmp(&score_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.position.cmp(&b.position))
            });
        }
        let selected = eligible_targets.first();
        Ok(RouteDecision {
            route_id: route.id.clone(),
            strategy: route.strategy.clone(),
            affinity_hash: affinity_hash.to_string(),
            candidates,
            ordered_targets: eligible_targets.clone(),
            selected_connection_id: selected.map(|target| target.connection_id.clone()),
            selected_model_id: selected.map(|target| target.model_id.clone()),
        })
    }
}

pub fn provider_profiles() -> Vec<super::profiles::ProviderProfile> {
    profiles()
        .into_iter()
        .filter(|profile| profile.id != "openai_legacy")
        .collect()
}

fn non_empty<'a>(value: &'a str, field: &str) -> Result<&'a str, LlmError> {
    let value = value.trim();
    if value.is_empty() {
        Err(LlmError::new(
            LlmErrorKind::InvalidRequest,
            format!("{field} cannot be empty."),
        ))
    } else {
        Ok(value)
    }
}
fn version_conflict(expected: u64, actual: u64) -> LlmError {
    let mut error = LlmError::new(
        LlmErrorKind::RouteConflict,
        format!("Version conflict: expected {expected}, current {actual}."),
    );
    error.causes.push(format!("expectedVersion={expected}"));
    error.causes.push(format!("actualVersion={actual}"));
    error
}
fn db_error(error: rusqlite::Error) -> LlmError {
    LlmError::new(
        LlmErrorKind::Persistence,
        format!("LLM persistence failure: {error}"),
    )
}
fn foreign_key_or_db(error: rusqlite::Error, message: &str) -> LlmError {
    if matches!(error,rusqlite::Error::SqliteFailure(ref code,_) if code.code==rusqlite::ErrorCode::ConstraintViolation)
    {
        LlmError::new(LlmErrorKind::RouteConflict, message)
    } else {
        db_error(error)
    }
}
pub fn endpoint_origin(endpoint: &str) -> String {
    reqwest::Url::parse(endpoint)
        .ok()
        .map(|url| {
            format!(
                "{}://{}{}",
                url.scheme(),
                url.host_str().unwrap_or("unknown"),
                url.port()
                    .map(|port| format!(":{port}"))
                    .unwrap_or_default()
            )
        })
        .unwrap_or_else(|| endpoint.to_string())
}

/// Stable envelope hashing regardless of `serde_json`'s `preserve_order` feature.
pub(crate) fn canonical_evidence_json(value: &Value) -> Result<String, LlmError> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(fields) => {
                let ordered = fields
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>();
                Value::Object(
                    ordered
                        .into_iter()
                        .map(|(key, value)| (key, sorted(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(sorted).collect()),
            value => value,
        }
    }
    let text = serde_json::to_string(&sorted(value.clone()))
        .map_err(|_| super::adapter::sanitization_error())?;
    if text.len() > crate::secret_redactor::MAX_SANITIZATION_BYTES {
        return Err(super::adapter::sanitization_error());
    }
    Ok(text)
}
