//! Explicit, per-connection discovery for HTTP/API providers.
//!
//! This module deliberately does not share the CLI catalog.  API discovery says only what an
//! API credential reported at refresh time; CLI accounts and their client-side model/profile
//! choices remain governed by the curated CLI contract.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use reqwest::Client;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::credentials::CredentialStore;
use super::error::{LlmError, LlmErrorKind};
use super::repository::SqliteLlmRepository;

const OPENAI_MODELS_URL: &str = "https://api.openai.com/v1/models";
const ANTHROPIC_MODELS_URL: &str = "https://api.anthropic.com/v1/models?limit=100";
const GEMINI_MODELS_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000";
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PAGES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiModelCatalogState {
    Ready,
    Stale,
    Failed,
    Unsupported,
}

impl ApiModelCatalogState {
    fn from_db(value: &str) -> Result<Self, LlmError> {
        match value {
            "ready" => Ok(Self::Ready),
            "stale" => Ok(Self::Stale),
            "failed" => Ok(Self::Failed),
            "unsupported" => Ok(Self::Unsupported),
            _ => Err(LlmError::new(
                LlmErrorKind::Persistence,
                "Invalid API catalog state.",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiModelCatalogEntry {
    pub model_id: String,
    pub display_name: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub capabilities: Vec<String>,
    /// Only values explicitly published by the provider. An empty list means "Not published".
    pub reasoning_efforts: Vec<String>,
    pub raw_metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiModelCatalogSnapshot {
    pub connection_id: String,
    pub provider_id: String,
    pub state: ApiModelCatalogState,
    pub source: String,
    pub diagnostic: Option<String>,
    pub refreshed_at: String,
    pub models: Vec<ApiModelCatalogEntry>,
}

#[derive(Clone)]
pub struct ApiModelCatalogService {
    repository: SqliteLlmRepository,
    credentials: Arc<dyn CredentialStore>,
    client: Client,
}

impl ApiModelCatalogService {
    pub fn new(
        repository: SqliteLlmRepository,
        credentials: Arc<dyn CredentialStore>,
        client: Client,
    ) -> Self {
        Self {
            repository,
            credentials,
            client,
        }
    }

    pub fn list(&self) -> Result<Vec<ApiModelCatalogSnapshot>, LlmError> {
        let connection = self.open_db()?;
        let mut statement = connection
            .prepare(
                "SELECT id,connection_id,provider_id,state,source,diagnostic,refreshed_at
             FROM api_model_catalog_snapshots ORDER BY refreshed_at DESC, connection_id",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(db_error)?;
        rows.map(|row| {
            let (id, connection_id, provider_id, state, source, diagnostic, refreshed_at) =
                row.map_err(db_error)?;
            Ok(ApiModelCatalogSnapshot {
                connection_id,
                provider_id,
                state: ApiModelCatalogState::from_db(&state)?,
                source,
                diagnostic,
                refreshed_at,
                models: self.entries(&connection, &id)?,
            })
        })
        .collect()
    }

    pub async fn refresh(&self, connection_id: &str) -> Result<ApiModelCatalogSnapshot, LlmError> {
        let resolved = self.repository.find_resolved_connection(connection_id)?;
        let provider = match resolved.provider_id.as_str() {
            "openai" | "openai_legacy" => "openai",
            "anthropic" => "anthropic",
            "gemini" => "gemini",
            _ => return Ok(self.unsupported(resolved.id.as_str(), resolved.provider_id.as_str())),
        };
        let credential = self
            .credentials
            .get(&resolved.credential_ref)?
            .ok_or_else(|| {
                LlmError::new(
                    LlmErrorKind::MissingApiKey,
                    "Connection has no API key to refresh its catalog.",
                )
            })?;
        let result = match provider {
            "openai" => self.fetch_openai(&credential).await,
            "anthropic" => self.fetch_anthropic(&credential).await,
            "gemini" => self.fetch_gemini(&credential).await,
            _ => unreachable!(),
        };
        match result {
            Ok(models) => {
                self.save_ready(resolved.id.as_str(), resolved.provider_id.as_str(), models)
            }
            Err(error) => Ok(self.stale_or_failed(
                resolved.id.as_str(),
                resolved.provider_id.as_str(),
                &error,
            )?),
        }
    }

    fn unsupported(&self, connection_id: &str, provider_id: &str) -> ApiModelCatalogSnapshot {
        ApiModelCatalogSnapshot {
            connection_id: connection_id.to_string(),
            provider_id: provider_id.to_string(),
            state: ApiModelCatalogState::Unsupported,
            source: "provider_api".into(),
            diagnostic: Some(
                "This provider does not yet publish an API catalog adapter in Ikore.".into(),
            ),
            refreshed_at: Utc::now().to_rfc3339(),
            models: Vec::new(),
        }
    }

    fn stale_or_failed(
        &self,
        connection_id: &str,
        provider_id: &str,
        error: &LlmError,
    ) -> Result<ApiModelCatalogSnapshot, LlmError> {
        let diagnostic = safe_diagnostic(error);
        let Some(mut snapshot) = self.load(connection_id)? else {
            return Ok(ApiModelCatalogSnapshot {
                connection_id: connection_id.into(),
                provider_id: provider_id.into(),
                state: ApiModelCatalogState::Failed,
                source: "provider_api".into(),
                diagnostic: Some(diagnostic),
                refreshed_at: Utc::now().to_rfc3339(),
                models: Vec::new(),
            });
        };
        snapshot.state = ApiModelCatalogState::Stale;
        snapshot.diagnostic = Some(diagnostic.clone());
        let connection = self.open_db()?;
        connection.execute(
            "UPDATE api_model_catalog_snapshots SET state='stale',diagnostic=?1 WHERE connection_id=?2",
            params![diagnostic, connection_id],
        ).map_err(db_error)?;
        Ok(snapshot)
    }

    fn save_ready(
        &self,
        connection_id: &str,
        provider_id: &str,
        models: Vec<ApiModelCatalogEntry>,
    ) -> Result<ApiModelCatalogSnapshot, LlmError> {
        let mut connection = self.open_db()?;
        let transaction = connection.transaction().map_err(db_error)?;
        let id = transaction
            .query_row(
                "SELECT id FROM api_model_catalog_snapshots WHERE connection_id=?1",
                params![connection_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO api_model_catalog_snapshots(id,connection_id,provider_id,state,source,diagnostic,refreshed_at,created_at)
             VALUES(?1,?2,?3,'ready','provider_api',NULL,?4,?4)
             ON CONFLICT(connection_id) DO UPDATE SET provider_id=excluded.provider_id,state='ready',source='provider_api',diagnostic=NULL,refreshed_at=excluded.refreshed_at",
            params![id, connection_id, provider_id, now],
        ).map_err(db_error)?;
        transaction
            .execute(
                "DELETE FROM api_model_catalog_entries WHERE snapshot_id=?1",
                params![id],
            )
            .map_err(db_error)?;
        for (position, entry) in models.iter().enumerate() {
            transaction.execute(
                "INSERT INTO api_model_catalog_entries(snapshot_id,model_id,display_name,context_window,max_output_tokens,capabilities_json,reasoning_efforts_json,raw_metadata_json,position)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![id, entry.model_id, entry.display_name, entry.context_window, entry.max_output_tokens,
                    serde_json::to_string(&entry.capabilities).unwrap_or_else(|_| "[]".into()),
                    serde_json::to_string(&entry.reasoning_efforts).unwrap_or_else(|_| "[]".into()),
                    serde_json::to_string(&entry.raw_metadata).unwrap_or_else(|_| "{}".into()), position as i64],
            ).map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        Ok(ApiModelCatalogSnapshot {
            connection_id: connection_id.into(),
            provider_id: provider_id.into(),
            state: ApiModelCatalogState::Ready,
            source: "provider_api".into(),
            diagnostic: None,
            refreshed_at: now,
            models,
        })
    }

    fn load(&self, connection_id: &str) -> Result<Option<ApiModelCatalogSnapshot>, LlmError> {
        let connection = self.open_db()?;
        let row = connection.query_row(
            "SELECT id,provider_id,state,source,diagnostic,refreshed_at FROM api_model_catalog_snapshots WHERE connection_id=?1",
            params![connection_id], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?,row.get::<_, String>(3)?,row.get::<_, Option<String>>(4)?,row.get::<_, String>(5)?)),
        ).optional().map_err(db_error)?;
        row.map(
            |(id, provider_id, state, source, diagnostic, refreshed_at)| {
                Ok(ApiModelCatalogSnapshot {
                    connection_id: connection_id.into(),
                    provider_id,
                    state: ApiModelCatalogState::from_db(&state)?,
                    source,
                    diagnostic,
                    refreshed_at,
                    models: self.entries(&connection, &id)?,
                })
            },
        )
        .transpose()
    }

    fn entries(
        &self,
        connection: &Connection,
        snapshot_id: &str,
    ) -> Result<Vec<ApiModelCatalogEntry>, LlmError> {
        let mut statement = connection.prepare(
            "SELECT model_id,display_name,context_window,max_output_tokens,capabilities_json,reasoning_efforts_json,raw_metadata_json
             FROM api_model_catalog_entries WHERE snapshot_id=?1 ORDER BY position,model_id",
        ).map_err(db_error)?;
        let rows = statement
            .query_map(params![snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<u64>>(2)?,
                    row.get::<_, Option<u64>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(db_error)?;
        rows.map(|row| {
            let (
                model_id,
                display_name,
                context_window,
                max_output_tokens,
                capabilities,
                efforts,
                raw,
            ) = row.map_err(db_error)?;
            Ok(ApiModelCatalogEntry {
                model_id,
                display_name,
                context_window,
                max_output_tokens,
                capabilities: serde_json::from_str(&capabilities).unwrap_or_default(),
                reasoning_efforts: serde_json::from_str(&efforts).unwrap_or_default(),
                raw_metadata: serde_json::from_str(&raw)
                    .unwrap_or(Value::Object(Default::default())),
            })
        })
        .collect()
    }

    fn open_db(&self) -> Result<Connection, LlmError> {
        let connection = Connection::open(self.repository.database_path()).map_err(db_error)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(db_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(db_error)?;
        Ok(connection)
    }

    async fn get_json(
        &self,
        url: &str,
        credential: &str,
        provider: &str,
    ) -> Result<Value, LlmError> {
        self.get_json_at(url, credential, provider).await
    }

    async fn get_json_at(
        &self,
        url: &str,
        credential: &str,
        provider: &str,
    ) -> Result<Value, LlmError> {
        let mut request = self.client.get(url);
        request = match provider {
            "openai" => request.bearer_auth(credential),
            "anthropic" => request
                .header("x-api-key", credential)
                .header("anthropic-version", "2023-06-01"),
            // Header authentication prevents the write-only key from becoming part of a URL.
            "gemini" => request.header("x-goog-api-key", credential),
            _ => request,
        };
        let response = request.send().await.map_err(|_| {
            LlmError::new(
                LlmErrorKind::Network,
                "Catalog request could not reach the provider.",
            )
        })?;
        if !response.status().is_success() {
            return Err(LlmError::new(
                LlmErrorKind::ProviderUnavailable,
                format!("Catalog request returned HTTP {}.", response.status()),
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES)
        {
            return Err(LlmError::new(
                LlmErrorKind::InvalidResponse,
                "Catalog response exceeded the safe size limit.",
            ));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| {
                LlmError::new(
                    LlmErrorKind::Network,
                    "Catalog response could not be read from the provider.",
                )
            })?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES as usize {
                return Err(LlmError::new(
                    LlmErrorKind::InvalidResponse,
                    "Catalog response exceeded the safe size limit.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            LlmError::new(
                LlmErrorKind::InvalidResponse,
                "Catalog response was not valid JSON.",
            )
        })
    }

    async fn fetch_openai(&self, credential: &str) -> Result<Vec<ApiModelCatalogEntry>, LlmError> {
        let value = self
            .get_json(OPENAI_MODELS_URL, credential, "openai")
            .await?;
        parse_openai(&value)
    }

    async fn fetch_anthropic(
        &self,
        credential: &str,
    ) -> Result<Vec<ApiModelCatalogEntry>, LlmError> {
        let mut url = ANTHROPIC_MODELS_URL.to_string();
        let mut entries = Vec::new();
        for _ in 0..MAX_PAGES {
            let value = self.get_json(&url, credential, "anthropic").await?;
            entries.extend(parse_anthropic(&value)?);
            let has_more = value
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let Some(after_id) = value
                .get("last_id")
                .and_then(Value::as_str)
                .filter(|_| has_more)
            else {
                break;
            };
            let mut parsed = reqwest::Url::parse(ANTHROPIC_MODELS_URL).map_err(|_| {
                LlmError::new(
                    LlmErrorKind::InvalidRequest,
                    "Invalid Anthropic catalog endpoint.",
                )
            })?;
            parsed.query_pairs_mut().append_pair("after_id", after_id);
            url = parsed.to_string();
        }
        Ok(deduplicate(entries))
    }

    async fn fetch_gemini(&self, credential: &str) -> Result<Vec<ApiModelCatalogEntry>, LlmError> {
        let mut url = GEMINI_MODELS_URL.to_string();
        let mut entries = Vec::new();
        for _ in 0..MAX_PAGES {
            let value = self.get_json(&url, credential, "gemini").await?;
            entries.extend(parse_gemini(&value)?);
            let Some(page_token) = value.get("nextPageToken").and_then(Value::as_str) else {
                break;
            };
            let mut parsed = reqwest::Url::parse(GEMINI_MODELS_URL).map_err(|_| {
                LlmError::new(
                    LlmErrorKind::InvalidRequest,
                    "Invalid Gemini catalog endpoint.",
                )
            })?;
            parsed
                .query_pairs_mut()
                .append_pair("pageToken", page_token);
            url = parsed.to_string();
        }
        Ok(deduplicate(entries))
    }
}

fn parse_openai(value: &Value) -> Result<Vec<ApiModelCatalogEntry>, LlmError> {
    let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
        LlmError::new(
            LlmErrorKind::InvalidResponse,
            "OpenAI catalog did not include data.",
        )
    })?;
    Ok(data.iter().filter_map(|model| {
        let id = model.get("id").and_then(Value::as_str)?.trim();
        (!id.is_empty()).then(|| ApiModelCatalogEntry {
            model_id: id.into(), display_name: id.into(), context_window: None, max_output_tokens: None, capabilities: Vec::new(), reasoning_efforts: Vec::new(),
            raw_metadata: json!({"created": model.get("created"), "owned_by": model.get("owned_by")}),
        })
    }).collect())
}

fn parse_anthropic(value: &Value) -> Result<Vec<ApiModelCatalogEntry>, LlmError> {
    let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
        LlmError::new(
            LlmErrorKind::InvalidResponse,
            "Anthropic catalog did not include data.",
        )
    })?;
    Ok(data
        .iter()
        .filter_map(|model| {
            let id = model.get("id").and_then(Value::as_str)?.trim();
            let display = model
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .trim();
            (!id.is_empty()).then(|| {
                let capabilities = declared_capabilities(model.get("capabilities"));
                let efforts = declared_efforts(model.get("capabilities"));
                ApiModelCatalogEntry {
                    model_id: id.into(),
                    display_name: display.into(),
                    context_window: number(
                        model,
                        &[
                            "context_window",
                            "context_window_tokens",
                            "input_token_limit",
                        ],
                    ),
                    max_output_tokens: number(model, &["max_output_tokens", "output_token_limit"]),
                    capabilities,
                    reasoning_efforts: efforts,
                    raw_metadata: minimized(model, &["type", "created_at"]),
                }
            })
        })
        .collect())
}

fn parse_gemini(value: &Value) -> Result<Vec<ApiModelCatalogEntry>, LlmError> {
    let data = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            LlmError::new(
                LlmErrorKind::InvalidResponse,
                "Gemini catalog did not include models.",
            )
        })?;
    Ok(data
        .iter()
        .filter_map(|model| {
            let raw_id = model.get("name").and_then(Value::as_str)?.trim();
            let id = raw_id.trim_start_matches("models/");
            (!id.is_empty()).then(|| ApiModelCatalogEntry {
                model_id: id.into(),
                display_name: model
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .into(),
                context_window: number(model, &["inputTokenLimit"]),
                max_output_tokens: number(model, &["outputTokenLimit"]),
                capabilities: string_array(model.get("supportedGenerationMethods"))
                    .into_iter()
                    .chain(string_array(model.get("supportedActions")))
                    .collect(),
                reasoning_efforts: Vec::new(),
                raw_metadata: minimized(model, &["description", "version", "baseModelId"]),
            })
        })
        .collect())
}

fn number(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(Value::as_u64).or_else(|| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .and_then(|v| v.parse().ok())
        })
    })
}
fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
fn declared_capabilities(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter(|&(_, value)| {
                    value == &Value::Bool(true)
                        || value.get("supported").and_then(Value::as_bool) == Some(true)
                })
                .map(|(key, _)| key.clone())
                .collect()
        })
        .unwrap_or_default()
}
fn declared_efforts(value: Option<&Value>) -> Vec<String> {
    let Some(capabilities) = value else {
        return Vec::new();
    };
    let effort = capabilities
        .get("effort")
        .or_else(|| capabilities.get("reasoning_effort"))
        .or_else(|| capabilities.get("supported_effort_levels"));
    let mut efforts = match effort {
        Some(Value::Array(_)) => string_array(effort),
        Some(Value::Object(map)) => map
            .iter()
            .filter(|&(_, value)| {
                value == &Value::Bool(true)
                    || value.get("supported").and_then(Value::as_bool) == Some(true)
            })
            .map(|(key, _)| key.clone())
            .collect(),
        Some(Value::String(value)) => vec![value.clone()],
        _ => Vec::new(),
    };
    efforts.sort();
    efforts
}
fn minimized(value: &Value, keys: &[&str]) -> Value {
    let mut result = serde_json::Map::new();
    for key in keys {
        if let Some(value) = value.get(*key) {
            result.insert((*key).into(), value.clone());
        }
    }
    Value::Object(result)
}
fn deduplicate(mut entries: Vec<ApiModelCatalogEntry>) -> Vec<ApiModelCatalogEntry> {
    entries.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    entries.dedup_by(|a, b| a.model_id == b.model_id);
    entries
}
fn safe_diagnostic(error: &LlmError) -> String {
    match error.kind {
        LlmErrorKind::MissingApiKey => "No API key is stored for this connection.".into(),
        LlmErrorKind::Network => "Catalog request could not reach the provider.".into(),
        _ => error.message.chars().take(240).collect(),
    }
}
fn db_error(error: rusqlite::Error) -> LlmError {
    LlmError::new(
        LlmErrorKind::Persistence,
        format!("API catalog storage error: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_fixtures_without_inventing_reasoning() {
        let openai =
            parse_openai(&json!({"data":[{"id":"gpt-fixture","owned_by":"fixture","created":1}]}))
                .unwrap();
        assert_eq!(openai[0].model_id, "gpt-fixture");
        assert!(openai[0].reasoning_efforts.is_empty());
        let anthropic = parse_anthropic(&json!({"data":[{"id":"claude-fixture","display_name":"Claude Fixture","context_window":200000,"max_output_tokens":8192,"capabilities":{"citations":true,"effort":{"low":true,"high":true}}}]})).unwrap();
        assert_eq!(anthropic[0].reasoning_efforts, ["high", "low"]);
        assert_eq!(anthropic[0].capabilities, ["citations"]);
        let gemini = parse_gemini(&json!({"models":[{"name":"models/gemini-fixture","displayName":"Gemini Fixture","inputTokenLimit":1000,"outputTokenLimit":100,"supportedGenerationMethods":["generateContent"]}]})).unwrap();
        assert_eq!(gemini[0].model_id, "gemini-fixture");
        assert_eq!(gemini[0].context_window, Some(1000));
    }

    #[tokio::test]
    async fn loopback_fixture_uses_header_auth_and_never_serializes_the_key() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/models", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 2048];
            let read = socket.read(&mut bytes).await.unwrap();
            let headers = String::from_utf8_lossy(&bytes[..read]);
            assert!(headers.contains("authorization: Bearer fixture-secret"));
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"data\":[{\"id\":\"gpt-fixture\"}]}").await.unwrap();
        });
        let service = ApiModelCatalogService::new(
            SqliteLlmRepository::new(":memory:"),
            Arc::new(crate::llm::credentials::KeyringCredentialStore),
            Client::new(),
        );
        let value = service
            .get_json_at(&url, "fixture-secret", "openai")
            .await
            .unwrap();
        server.await.unwrap();
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains("fixture-secret"));
    }

    #[test]
    fn failed_refresh_keeps_the_previous_snapshot_stale_and_secret_free() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE provider_connections(id TEXT PRIMARY KEY);
             CREATE TABLE api_model_catalog_snapshots(id TEXT PRIMARY KEY,connection_id TEXT NOT NULL UNIQUE,provider_id TEXT NOT NULL,state TEXT NOT NULL,source TEXT NOT NULL,diagnostic TEXT,refreshed_at TEXT NOT NULL,created_at TEXT NOT NULL);
             CREATE TABLE api_model_catalog_entries(snapshot_id TEXT NOT NULL,model_id TEXT NOT NULL,display_name TEXT NOT NULL,context_window INTEGER,max_output_tokens INTEGER,capabilities_json TEXT NOT NULL,reasoning_efforts_json TEXT NOT NULL,raw_metadata_json TEXT NOT NULL,position INTEGER NOT NULL,PRIMARY KEY(snapshot_id,model_id));
             INSERT INTO provider_connections VALUES ('connection-a');",
        ).unwrap();
        drop(db);
        let service = ApiModelCatalogService::new(
            SqliteLlmRepository::new(&path),
            Arc::new(crate::llm::credentials::KeyringCredentialStore),
            Client::new(),
        );
        service
            .save_ready(
                "connection-a",
                "openai",
                vec![ApiModelCatalogEntry {
                    model_id: "gpt-fixture".into(),
                    display_name: "GPT Fixture".into(),
                    context_window: None,
                    max_output_tokens: None,
                    capabilities: vec![],
                    reasoning_efforts: vec![],
                    raw_metadata: json!({}),
                }],
            )
            .unwrap();
        let stale = service
            .stale_or_failed(
                "connection-a",
                "openai",
                &LlmError::new(
                    LlmErrorKind::Network,
                    "https://example.invalid/?key=fixture-secret",
                ),
            )
            .unwrap();
        assert_eq!(stale.state, ApiModelCatalogState::Stale);
        assert_eq!(stale.models.len(), 1);
        assert!(!stale.diagnostic.unwrap().contains("fixture-secret"));
    }
}
