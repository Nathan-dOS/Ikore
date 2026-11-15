//! The durable work of every project at a glance, for the shell's projects sidebar.
//!
//! Cross-project by design, and read-only: the sidebar shows what runs or waits in the projects
//! that are not open. It reads short lists from the global database and never opens a workspace.

use std::collections::HashMap;

use rusqlite::{Connection, params};
use serde::Serialize;

use super::{PersistenceService, PersistenceServiceError, Project};

/// What a session is doing. Ordered by how much it asks of the developer, so the largest of a
/// project's sessions is the project's own mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Idle,
    Done,
    Failed,
    Running,
    /// A decision of the developer is pending.
    Waiting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Pulsation,
    Feature,
    Conversation,
}

/// One durable piece of work: a pulsation, a Forge feature or a Companion conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    pub kind: SessionKind,
    pub id: String,
    pub title: String,
    /// The stage of a pulsation, the state of a feature, the model of a conversation.
    pub detail: String,
    pub state: SessionState,
    /// A conversation the developer pinned; it comes before the others of its project.
    pub pinned: bool,
    pub updated_at: String,
}

/// A project and its most recent sessions of each kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSessions {
    pub project: Project,
    /// Pulsations, then features, then conversations; the most recent first within each.
    pub sessions: Vec<SessionEntry>,
    /// `Waiting` or `Running` when any session of the project is, listed here or not. Finished
    /// and failed work never marks a project: nothing records that the developer has seen it.
    pub state: SessionState,
}

fn pulsation_state(status: &str) -> SessionState {
    match status {
        "checkpoint" => SessionState::Waiting,
        "planning" | "running" => SessionState::Running,
        "failed" => SessionState::Failed,
        "done" => SessionState::Done,
        _ => SessionState::Idle,
    }
}

fn feature_state(state: &str) -> SessionState {
    match state {
        "ready_for_review" => SessionState::Waiting,
        "executing" => SessionState::Running,
        "done" => SessionState::Done,
        _ => SessionState::Idle,
    }
}

impl PersistenceService {
    /// Every project that is not archived, in the order the projects list uses, each with its
    /// `per_kind` most recent pulsations, features and conversations.
    pub fn project_sessions(
        &self,
        per_kind: usize,
    ) -> Result<Vec<ProjectSessions>, PersistenceServiceError> {
        let connection = self.connection()?;
        let mut sessions = recent_sessions(&connection, per_kind)?;
        let mut live = live_projects(&connection)?;
        let mut statement = connection.prepare(
            "SELECT id, name, workspace_path, is_archived, created_at, updated_at, last_opened_at
             FROM projects
             WHERE is_archived = 0
             ORDER BY
               CASE WHEN last_opened_at IS NULL THEN 1 ELSE 0 END ASC,
               last_opened_at DESC,
               updated_at DESC",
        )?;
        let projects = statement
            .query_map([], Self::map_project)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(projects
            .into_iter()
            .map(|project| ProjectSessions {
                sessions: sessions.remove(&project.id).unwrap_or_default(),
                state: live.remove(&project.id).unwrap_or(SessionState::Idle),
                project,
            })
            .collect())
    }
}

/// The most recent sessions of each kind, by project.
fn recent_sessions(
    connection: &Connection,
    per_kind: usize,
) -> Result<HashMap<String, Vec<SessionEntry>>, PersistenceServiceError> {
    let limit = i64::try_from(per_kind).unwrap_or(i64::MAX);
    let mut sessions: HashMap<String, Vec<SessionEntry>> = HashMap::new();
    let mut read = |sql: &str,
                    entry: &dyn Fn(&rusqlite::Row<'_>) -> rusqlite::Result<SessionEntry>|
     -> Result<(), PersistenceServiceError> {
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map(params![limit], |row| {
            Ok((row.get::<_, String>("project_id")?, entry(row)?))
        })?;
        for row in rows {
            let (project_id, entry) = row?;
            sessions.entry(project_id).or_default().push(entry);
        }
        Ok(())
    };
    read(
        "SELECT id, project_id, feature_name, stage, status, updated_at FROM (
           SELECT p.*, ROW_NUMBER() OVER (
             PARTITION BY project_id ORDER BY updated_at DESC, id) AS position
           FROM pulsations p)
         WHERE position <= ?1
         ORDER BY project_id, position",
        &|row| {
            Ok(SessionEntry {
                kind: SessionKind::Pulsation,
                id: row.get("id")?,
                title: row.get("feature_name")?,
                detail: row.get("stage")?,
                state: pulsation_state(&row.get::<_, String>("status")?),
                pinned: false,
                updated_at: row.get("updated_at")?,
            })
        },
    )?;
    read(
        "SELECT id, project_id, title, state, updated_at FROM (
           SELECT f.*, ROW_NUMBER() OVER (
             PARTITION BY project_id ORDER BY updated_at DESC, id) AS position
           FROM forge_features f
           WHERE state <> 'archived')
         WHERE position <= ?1
         ORDER BY project_id, position",
        &|row| {
            let state: String = row.get("state")?;
            Ok(SessionEntry {
                kind: SessionKind::Feature,
                id: row.get("id")?,
                title: row.get("title")?,
                state: feature_state(&state),
                detail: state.replace('_', " "),
                pinned: false,
                updated_at: row.get("updated_at")?,
            })
        },
    )?;
    // Pins and archives are the Companion's marks (`chat_service::sessions`): pinned
    // conversations come first, archived ones are left out.
    read(
        "SELECT id, project_id, title, model_id_snapshot, updated_at, pinned,
                EXISTS(SELECT 1 FROM chat_messages m
                       WHERE m.conversation_id = ranked.id
                         AND m.status IN ('pending', 'streaming')) AS replying
         FROM (
           SELECT marked.*, ROW_NUMBER() OVER (
             PARTITION BY project_id ORDER BY pinned DESC, updated_at DESC, id) AS position
           FROM (
             SELECT c.id, c.project_id, c.title, c.model_id_snapshot, c.updated_at,
                    EXISTS(SELECT 1 FROM runtime_settings s,
                             json_each(CASE WHEN json_valid(s.value) THEN s.value ELSE '{}' END,
                                       '$.pinned') p
                           WHERE s.key = 'companion.marks.' || c.project_id
                             AND p.value = c.id) AS pinned
             FROM chat_conversations c
             WHERE NOT EXISTS(SELECT 1 FROM runtime_settings s,
                                json_each(CASE WHEN json_valid(s.value) THEN s.value ELSE '{}' END,
                                          '$.archived') a
                              WHERE s.key = 'companion.marks.' || c.project_id
                                AND a.value = c.id)) marked) ranked
         WHERE position <= ?1
         ORDER BY project_id, position",
        &|row| {
            Ok(SessionEntry {
                kind: SessionKind::Conversation,
                id: row.get("id")?,
                title: row.get("title")?,
                detail: row.get("model_id_snapshot")?,
                state: if row.get::<_, bool>("replying")? {
                    SessionState::Running
                } else {
                    SessionState::Idle
                },
                pinned: row.get("pinned")?,
                updated_at: row.get("updated_at")?,
            })
        },
    )?;
    Ok(sessions)
}

/// The projects with something running or waiting, whether or not it is among the recent
/// sessions listed.
fn live_projects(
    connection: &Connection,
) -> Result<HashMap<String, SessionState>, PersistenceServiceError> {
    let mut live: HashMap<String, SessionState> = HashMap::new();
    let mut mark = |sql: &str, state: SessionState| -> Result<(), PersistenceServiceError> {
        let mut statement = connection.prepare(sql)?;
        for project_id in statement.query_map([], |row| row.get::<_, String>(0))? {
            let marked = live.entry(project_id?).or_insert(state);
            *marked = (*marked).max(state);
        }
        Ok(())
    };
    mark(
        "SELECT DISTINCT project_id FROM pulsations WHERE status IN ('planning', 'running')",
        SessionState::Running,
    )?;
    mark(
        "SELECT DISTINCT project_id FROM forge_features WHERE state = 'executing'",
        SessionState::Running,
    )?;
    mark(
        "SELECT DISTINCT c.project_id FROM chat_conversations c
         JOIN chat_messages m ON m.conversation_id = c.id
         WHERE m.status IN ('pending', 'streaming')",
        SessionState::Running,
    )?;
    mark(
        "SELECT DISTINCT project_id FROM pulsations WHERE status = 'checkpoint'",
        SessionState::Waiting,
    )?;
    mark(
        "SELECT DISTINCT project_id FROM forge_features WHERE state = 'ready_for_review'",
        SessionState::Waiting,
    )?;
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence_service::{CreatePulsationRequest, EnsureProjectForWorkspaceRequest};

    struct Fixture {
        _data: tempfile::TempDir,
        _workspaces: Vec<tempfile::TempDir>,
        service: PersistenceService,
    }

    impl Fixture {
        fn new() -> Self {
            let data = tempfile::tempdir().unwrap();
            let service = PersistenceService::initialize(data.path()).unwrap();
            Self {
                _data: data,
                _workspaces: Vec::new(),
                service,
            }
        }

        fn project(&mut self, name: &str) -> String {
            let workspace = tempfile::tempdir().unwrap();
            let project = self
                .service
                .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                    workspace_path: workspace.path().to_string_lossy().into_owned(),
                    preferred_name: Some(name.into()),
                })
                .unwrap()
                .project;
            self._workspaces.push(workspace);
            project.id
        }

        fn pulsation(&self, project: &str, name: &str, status: &str, updated_at: &str) -> String {
            let pulse = self
                .service
                .create_pulsation(CreatePulsationRequest {
                    project_id: project.into(),
                    feature_name: name.into(),
                })
                .unwrap()
                .pulsation;
            self.sql(
                "UPDATE pulsations SET status = ?2, updated_at = ?3 WHERE id = ?1",
                params![pulse.id, status, updated_at],
            );
            pulse.id
        }

        fn sql(&self, sql: &str, params: impl rusqlite::Params) {
            self.service
                .connection()
                .unwrap()
                .execute(sql, params)
                .unwrap();
        }

        fn conversation(&self, project: &str, id: &str, reply: Option<&str>) {
            self.sql(
                "INSERT INTO chat_conversations
                   (id, project_id, provider_id_snapshot, model_id_snapshot, title,
                    created_at, updated_at)
                 VALUES (?1, ?2, 'openai', 'gpt-5', ?1, '2026-11-01', '2026-11-01')",
                params![id, project],
            );
            if let Some(status) = reply {
                self.sql(
                    "INSERT INTO chat_messages
                       (id, conversation_id, ordinal, role, status, created_at, updated_at)
                     VALUES (?1, ?2, 1, 'assistant', ?3, '2026-11-01', '2026-11-01')",
                    params![format!("{id}-reply"), id, status],
                );
            }
        }

        fn feature(&self, project: &str, id: &str, state: &str) {
            self.sql(
                "INSERT INTO forge_features
                   (id, project_id, title, original_idea, state, brief_json, evaluation_json,
                    created_at, updated_at)
                 VALUES (?1, ?2, ?1, 'idea', ?3, '{}', '{}', '2026-11-01', '2026-11-01')",
                params![id, project, state],
            );
        }
    }

    fn listed(project: &ProjectSessions) -> Vec<String> {
        project
            .sessions
            .iter()
            .map(|session| format!("{} {:?}", session.title, session.state))
            .collect()
    }

    #[test]
    fn every_project_lists_its_recent_sessions_and_says_what_is_live() {
        let mut fixture = Fixture::new();
        let quiet = fixture.project("quiet");
        let busy = fixture.project("busy");
        let empty = fixture.project("empty");
        // Three pulsations, the oldest one waiting: it falls out of a list of two.
        fixture.pulsation(
            &busy,
            "old review",
            "checkpoint",
            "2026-11-01T10:00:00+00:00",
        );
        fixture.pulsation(&busy, "finished", "done", "2026-11-02T10:00:00+00:00");
        fixture.pulsation(&busy, "in flight", "running", "2026-11-03T10:00:00+00:00");
        fixture.feature(&busy, "feature-a", "executing");
        fixture.feature(&busy, "feature-gone", "archived");
        fixture.conversation(&busy, "chat-a", Some("streaming"));
        fixture.pulsation(&quiet, "broke", "failed", "2026-11-03T10:00:00+00:00");
        fixture.conversation(&quiet, "chat-b", Some("completed"));

        let projects = fixture.service.project_sessions(2).unwrap();
        let by_id = |id: &str| {
            projects
                .iter()
                .find(|entry| entry.project.id == id)
                .unwrap()
        };
        assert_eq!(projects.len(), 3);
        assert_eq!(
            listed(by_id(&busy)),
            [
                "in flight Running",
                "finished Done",
                "feature-a Running",
                "chat-a Running",
            ]
        );
        // The waiting pulsation is not among the two listed, and still marks its project.
        assert_eq!(by_id(&busy).state, SessionState::Waiting);
        let details: Vec<&str> = by_id(&busy)
            .sessions
            .iter()
            .map(|session| session.detail.as_str())
            .collect();
        assert_eq!(details[2..], ["executing", "gpt-5"]);

        // Failed and finished work shows on its row and does not mark the project.
        assert_eq!(listed(by_id(&quiet)), ["broke Failed", "chat-b Idle"]);
        assert_eq!(by_id(&quiet).state, SessionState::Idle);
        assert!(by_id(&empty).sessions.is_empty());
        assert_eq!(by_id(&empty).state, SessionState::Idle);
    }

    #[test]
    fn pinned_conversations_come_first_and_archived_ones_are_left_out() {
        let mut fixture = Fixture::new();
        let project = fixture.project("marked");
        for id in ["chat-old", "chat-mid", "chat-new"] {
            fixture.conversation(&project, id, None);
        }
        for (id, updated_at) in [
            ("chat-old", "2026-11-01T10:00:00+00:00"),
            ("chat-mid", "2026-11-02T10:00:00+00:00"),
            ("chat-new", "2026-11-03T10:00:00+00:00"),
        ] {
            fixture.sql(
                "UPDATE chat_conversations SET updated_at = ?2 WHERE id = ?1",
                params![id, updated_at],
            );
        }
        fixture.sql(
            "INSERT INTO runtime_settings(key, value, updated_at) VALUES (?1, ?2, 'now')",
            params![
                format!("companion.marks.{project}"),
                r#"{"pinned":["chat-old"],"archived":["chat-new"]}"#
            ],
        );
        let projects = fixture.service.project_sessions(5).unwrap();
        let sessions: Vec<_> = projects[0]
            .sessions
            .iter()
            .map(|session| (session.id.as_str(), session.pinned))
            .collect();
        assert_eq!(sessions, [("chat-old", true), ("chat-mid", false)]);

        // Marks that are not JSON never break the sidebar.
        fixture.sql(
            "UPDATE runtime_settings SET value = 'not json' WHERE key = ?1",
            params![format!("companion.marks.{project}")],
        );
        assert_eq!(
            fixture.service.project_sessions(5).unwrap()[0]
                .sessions
                .len(),
            3
        );
    }

    #[test]
    fn an_archived_project_is_left_out() {
        let mut fixture = Fixture::new();
        let kept = fixture.project("kept");
        let archived = fixture.project("archived");
        fixture.sql(
            "UPDATE projects SET is_archived = 1 WHERE id = ?1",
            params![archived],
        );
        let projects = fixture.service.project_sessions(5).unwrap();
        assert_eq!(
            projects
                .iter()
                .map(|entry| entry.project.id.as_str())
                .collect::<Vec<_>>(),
            [kept.as_str()]
        );
    }
}
