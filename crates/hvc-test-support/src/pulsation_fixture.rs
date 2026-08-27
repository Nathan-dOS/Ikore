use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{Connection, params};
use serde_json::json;

use crate::{DatabaseVersion, IsolatedTestEnvironment};

#[derive(Debug, Clone, Copy)]
pub enum LegacyPulseLayout {
    TwoQueued,
    ThreeQueued,
    OneActive,
    MultipleActive,
}

/// V9 data only: no AppCore, worker, real profile or provider is started.
/// The same file path exists in two repositories without a vault or AST parser.
pub struct PulsationFixture {
    environment: IsolatedTestEnvironment,
}

impl PulsationFixture {
    pub fn new(layout: LegacyPulseLayout) -> Self {
        let environment = IsolatedTestEnvironment::new();
        environment.prepare_database(DatabaseVersion::new(9));
        let fixture = Self { environment };
        let mut connection = fixture.connection();
        let transaction = connection.transaction().expect("fixture transaction");
        let timestamp = "2026-08-27T12:00:00Z";
        for project in ["project-a", "project-b"] {
            let root = fixture.environment.workspace_dir().join(project);
            fs::create_dir_all(root.join("src")).expect("fixture source directory");
            fs::write(
                root.join("src/auth.rs"),
                "pub fn allowed() -> bool { false }\r\n",
            )
            .expect("fixture source");
            transaction.execute(
                "INSERT INTO projects (id,name,workspace_path,created_at,updated_at) VALUES (?1,?1,?2,?3,?3)",
                params![project, root.to_string_lossy(), timestamp],
            ).expect("fixture project");
        }
        let count = if matches!(layout, LegacyPulseLayout::TwoQueued) {
            2
        } else {
            3
        };
        for (index, pulse) in ["pulse-a", "pulse-b", "pulse-c", "pulse-other"]
            .iter()
            .enumerate()
        {
            if index == 2 && count == 2 {
                continue;
            }
            let active = (index == 0
                && matches!(
                    layout,
                    LegacyPulseLayout::OneActive | LegacyPulseLayout::MultipleActive
                ))
                || (index == 1 && matches!(layout, LegacyPulseLayout::MultipleActive));
            let stage = if active { "SPECIFICATION" } else { "Intake" };
            let status = if active { "checkpoint" } else { "planning" };
            let project = if index == 3 { "project-b" } else { "project-a" };
            let job = format!("job-{pulse}");
            transaction.execute(
                "INSERT INTO pulsations (id,project_id,feature_name,stage,status,created_at,updated_at) VALUES (?1,?2,?1,?3,?4,?5,?5)",
                params![pulse, project, stage, status, timestamp],
            ).expect("fixture pulsation");
            transaction.execute(
                "INSERT INTO jobs (id,pulsation_id,kind,status,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?5)",
                params![job, pulse, format!("run_stage::{stage}"), if active { "done" } else { "pending" }, timestamp],
            ).expect("fixture job");
            if active {
                transaction.execute(
                    "INSERT INTO checkpoints (id,pulsation_id,stage,status,created_at,updated_at) VALUES (?1,?2,'SPECIFICATION','waiting',?3,?3)",
                    params![format!("checkpoint-{pulse}"), pulse, timestamp],
                ).expect("fixture checkpoint");
                let run = format!("run-{pulse}");
                transaction.execute(
                    "INSERT INTO agent_runs (id,job_id,agent_name,status,created_at,updated_at) VALUES (?1,?2,'SPECIFICATION','done',?3,?3)",
                    params![run, job, timestamp],
                ).expect("fixture run");
                let output = json!({
                    "summary":"Legacy specification", "statusUpdate":"checkpoint_ready", "notes":[],
                    "artifacts":[{"name":"spec","kind":"specification","content":"# Legacy specification\n"}]
                }).to_string();
                transaction.execute(
                    "INSERT INTO agent_run_outputs (id,agent_run_id,job_id,pulsation_id,request_payload,response_payload,raw_output,structured_output,created_at,updated_at) VALUES (?1,?2,?3,?4,'{}','{}',?5,?5,?6,?6)",
                    params![format!("output-{pulse}"), run, job, pulse, output, timestamp],
                ).expect("fixture legacy output");
                transaction.execute(
                    "INSERT INTO artifacts (id,pulsation_id,name,kind,created_at,updated_at) VALUES (?1,?2,'spec','specification',?3,?3)",
                    params![format!("artifact-{pulse}"), pulse, timestamp],
                ).expect("fixture legacy artifact metadata");
            }
        }
        transaction.commit().expect("fixture commit");
        fixture
    }

    pub fn database_path(&self) -> PathBuf {
        self.environment.data_dir().join("hard-vibe-code.db")
    }

    pub fn project_a_root(&self) -> PathBuf {
        self.environment.workspace_dir().join("project-a")
    }

    pub fn project_b_root(&self) -> PathBuf {
        self.environment.workspace_dir().join("project-b")
    }

    pub fn connection(&self) -> Connection {
        let connection = Connection::open(self.database_path()).expect("fixture database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("fixture foreign keys");
        connection
            .busy_timeout(Duration::from_secs(5))
            .expect("fixture busy timeout");
        connection
    }
}
