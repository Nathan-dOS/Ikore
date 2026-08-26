use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hvc_core::{AppCore, AppCoreDependencies, AppPaths};
use refinery::Target;
use rusqlite::Connection;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use crate::{FixedEmbeddingGateway, FixedJitter, ManualClock, MemoryCredentialStore};

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("../hvc-core/migrations");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseVersion(u32);

impl DatabaseVersion {
    pub fn new(version: u32) -> Self {
        assert!((1..=9).contains(&version), "fixture version must be V1-V9");
        Self(version)
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

pub struct IsolatedTestEnvironment {
    _root: TempDir,
    data_dir: PathBuf,
    workspace_dir: PathBuf,
}

impl IsolatedTestEnvironment {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("isolated test root should be created");
        let data_dir = root.path().join("data");
        let workspace_dir = root.path().join("workspace");
        fs::create_dir_all(&data_dir).expect("isolated data directory should be created");
        fs::create_dir_all(workspace_dir.join("notes"))
            .expect("isolated workspace should be created");
        fs::write(
            workspace_dir.join("notes/index.md"),
            "---\ntitle: Fixture\n---\n\n# Fixture\n\n[[second]]\n",
        )
        .expect("fixture index should be written");
        fs::write(workspace_dir.join("notes/second.md"), "# Second\n")
            .expect("fixture note should be written");
        Self {
            _root: root,
            data_dir,
            workspace_dir,
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    pub fn prepare_database(&self, version: DatabaseVersion) {
        let db_path = self.data_dir.join("hard-vibe-code.db");
        let mut connection = Connection::open(db_path).expect("fixture database should open");
        embedded::migrations::runner()
            .set_target(Target::Version(version.get()))
            .run(&mut connection)
            .expect("fixture migrations should apply");
    }
}

impl Default for IsolatedTestEnvironment {
    fn default() -> Self {
        Self::new()
    }
}

pub struct CoreTestHarness {
    runtime: Option<Runtime>,
    core: Arc<AppCore>,
    environment: IsolatedTestEnvironment,
    credentials: Arc<MemoryCredentialStore>,
    clock: Arc<ManualClock>,
    embeddings: Arc<FixedEmbeddingGateway>,
}

impl CoreTestHarness {
    pub fn new() -> Self {
        Self::build(None)
    }

    pub fn at_database_version(version: DatabaseVersion) -> Self {
        Self::build(Some(version))
    }

    fn build(version: Option<DatabaseVersion>) -> Self {
        let environment = IsolatedTestEnvironment::new();
        if let Some(version) = version {
            environment.prepare_database(version);
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("hvc-test-runtime")
            .build()
            .expect("test runtime should initialize");
        let credentials = Arc::new(MemoryCredentialStore::default());
        let clock = Arc::new(ManualClock::new(fixed_instant()));
        let embeddings = Arc::new(FixedEmbeddingGateway::default());
        let dependencies = AppCoreDependencies {
            credentials: credentials.clone(),
            embedding_gateway: embeddings.clone(),
            clock: clock.clone(),
            jitter: Arc::new(FixedJitter::default()),
            ..AppCoreDependencies::default()
        };
        let core = AppCore::initialize_with_dependencies(
            AppPaths::from_data_dir(environment.data_dir()),
            runtime.handle().clone(),
            dependencies,
        )
        .expect("isolated AppCore should initialize");
        Self {
            runtime: Some(runtime),
            core,
            environment,
            credentials,
            clock,
            embeddings,
        }
    }

    pub fn core(&self) -> Arc<AppCore> {
        Arc::clone(&self.core)
    }

    pub fn data_dir(&self) -> &Path {
        self.environment.data_dir()
    }

    pub fn workspace_dir(&self) -> &Path {
        self.environment.workspace_dir()
    }

    pub fn credentials(&self) -> Arc<MemoryCredentialStore> {
        Arc::clone(&self.credentials)
    }

    pub fn clock(&self) -> Arc<ManualClock> {
        Arc::clone(&self.clock)
    }

    pub fn embeddings(&self) -> Arc<FixedEmbeddingGateway> {
        Arc::clone(&self.embeddings)
    }
}

impl Default for CoreTestHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CoreTestHarness {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.block_on(self.core.shutdown());
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

fn fixed_instant() -> DateTime<Utc> {
    "2026-08-23T12:00:00Z"
        .parse()
        .expect("fixed test instant should parse")
}

#[cfg(test)]
mod tests {
    use hvc_core::llm::{Clock, CredentialStore};

    use super::*;

    #[test]
    fn harness_uses_isolated_data_and_in_memory_dependencies() {
        let harness = CoreTestHarness::new();
        harness
            .credentials()
            .set("provider-a", "fixture-secret")
            .unwrap();

        assert!(harness.data_dir().join("hard-vibe-code.db").is_file());
        assert!(harness.workspace_dir().join("notes/index.md").is_file());
        assert!(harness.credentials().contains("provider-a"));
        assert_eq!(
            harness.clock().now().to_rfc3339(),
            "2026-08-23T12:00:00+00:00"
        );
        assert_eq!(harness.embeddings().call_count(), 0);
    }

    #[test]
    fn v1_fixture_is_upgraded_by_app_core_to_v9() {
        let harness = CoreTestHarness::at_database_version(DatabaseVersion::new(1));
        let connection = Connection::open(harness.data_dir().join("hard-vibe-code.db")).unwrap();
        let chat_table: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='chat_messages')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(chat_table);
    }
}
