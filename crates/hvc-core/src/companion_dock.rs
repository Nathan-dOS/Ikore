//! Durable Companion layout and unsent drafts, separate from chat history.
use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::persistence_service::{PersistenceService, PersistenceServiceError};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanionDockLayout {
    pub open: bool,
    pub conversations: Vec<CompanionDockItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanionDockItem {
    pub topic_id: String,
    pub conversation_id: Option<String>,
    pub decision_id: Option<String>,
    pub minimized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanionDraft {
    pub topic_id: String,
    pub conversation_id: Option<String>,
    pub revision: u64,
    pub content: String,
    pub decision_id: Option<String>,
    pub decision_state_version: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionDockSnapshot {
    pub layout_revision: u64,
    pub layout: CompanionDockLayout,
    pub drafts: Vec<CompanionDraft>,
}

fn invalid(message: impl Into<String>) -> PersistenceServiceError {
    PersistenceServiceError::Runtime(message.into())
}

pub(crate) fn require_project(
    db: &Connection,
    project: &str,
) -> Result<(), PersistenceServiceError> {
    let found: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project],
        |row| row.get(0),
    )?;
    if found {
        Ok(())
    } else {
        Err(invalid("Project not found."))
    }
}

fn require_conversation(
    db: &Connection,
    project: &str,
    conversation: &str,
) -> Result<(), PersistenceServiceError> {
    let found = conversation_in_project(db, project, conversation)?;
    if found {
        Ok(())
    } else {
        Err(invalid("Conversation does not belong to this project."))
    }
}

fn conversation_in_project(
    db: &Connection,
    project: &str,
    conversation: &str,
) -> Result<bool, PersistenceServiceError> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_conversations WHERE id=?1 AND project_id=?2)",
        params![conversation, project],
        |row| row.get(0),
    )?)
}

fn validate_revision(revision: u64) -> Result<(), PersistenceServiceError> {
    if revision == 0 || revision > i64::MAX as u64 {
        return Err(invalid("Invalid Companion state revision."));
    }
    Ok(())
}

impl PersistenceService {
    pub fn load_companion_dock(
        &self,
        project: &str,
    ) -> Result<CompanionDockSnapshot, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        require_project(&tx, project)?;
        let mut snapshot = CompanionDockSnapshot::default();
        if let Some((revision, json)) = tx
            .query_row(
                "SELECT revision,layout_json FROM companion_dock_sessions WHERE project_id=?1",
                [project],
                |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            snapshot.layout_revision = revision;
            snapshot.layout = serde_json::from_str(&json)
                .map_err(|error| invalid(format!("Invalid Companion layout: {error}")))?;
            let mut present = Vec::new();
            for item in std::mem::take(&mut snapshot.layout.conversations) {
                if let Some(id) = &item.conversation_id
                    && !conversation_in_project(&tx, project, id)?
                {
                    continue;
                }
                present.push(item);
            }
            snapshot.layout.conversations = present;
        }
        snapshot.drafts = tx.prepare("SELECT topic_id,conversation_id,revision,content,decision_id,decision_state_version FROM companion_drafts WHERE project_id=?1 ORDER BY topic_id")?.query_map([project], |row| Ok(CompanionDraft {
            topic_id: row.get(0)?, conversation_id: row.get(1)?, revision: row.get(2)?, content: row.get(3)?, decision_id: row.get(4)?, decision_state_version: row.get(5)?,
        }))?.collect::<Result<Vec<_>, _>>()?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// A revision is monotonic per project; stale background saves do nothing.
    pub fn save_companion_dock(
        &self,
        project: &str,
        revision: u64,
        layout: &CompanionDockLayout,
    ) -> Result<bool, PersistenceServiceError> {
        validate_revision(revision)?;
        if layout.conversations.len() > 100 {
            return Err(invalid(
                "At most 100 conversations may be kept in the dock.",
            ));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        require_project(&tx, project)?;
        let mut seen = HashSet::new();
        for item in &layout.conversations {
            if item.topic_id.is_empty() || item.topic_id.len() > 256 || !seen.insert(&item.topic_id)
            {
                return Err(invalid("Invalid or repeated Companion topic."));
            }
            if let Some(id) = &item.conversation_id {
                require_conversation(&tx, project, id)?;
            }
            if item.decision_id.as_ref().is_some_and(|id| id.len() > 256) {
                return Err(invalid("Invalid Companion decision reference."));
            }
        }
        let json = serde_json::to_string(layout).map_err(|error| invalid(error.to_string()))?;
        let changed = tx.execute("INSERT INTO companion_dock_sessions(project_id,revision,layout_json,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(project_id) DO UPDATE SET revision=excluded.revision,layout_json=excluded.layout_json,updated_at=excluded.updated_at WHERE excluded.revision>companion_dock_sessions.revision", params![project, revision, json, chrono::Utc::now().to_rfc3339()])?;
        tx.commit()?;
        Ok(changed != 0)
    }

    /// Empty content is a versioned tombstone, preventing an old debounce from
    /// restoring text after a successful Send.
    pub fn save_companion_draft(
        &self,
        project: &str,
        draft: &CompanionDraft,
    ) -> Result<bool, PersistenceServiceError> {
        validate_revision(draft.revision)?;
        if draft.topic_id.is_empty()
            || draft.topic_id.len() > 256
            || draft.content.chars().count() > 20_000
            || draft.decision_id.as_ref().is_some_and(|id| id.len() > 256)
            || draft
                .decision_state_version
                .is_some_and(|version| version > i64::MAX as u64)
        {
            return Err(invalid("Companion draft exceeds its allowed limits."));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        require_project(&tx, project)?;
        if let Some(id) = &draft.conversation_id {
            require_conversation(&tx, project, id)?;
        }
        let changed = tx.execute("INSERT INTO companion_drafts(topic_id,conversation_id,project_id,revision,content,decision_id,decision_state_version,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(project_id,topic_id) DO UPDATE SET conversation_id=excluded.conversation_id,revision=excluded.revision,content=excluded.content,decision_id=excluded.decision_id,decision_state_version=excluded.decision_state_version,updated_at=excluded.updated_at WHERE excluded.revision>companion_drafts.revision", params![draft.topic_id, draft.conversation_id, project, draft.revision, draft.content, draft.decision_id, draft.decision_state_version, chrono::Utc::now().to_rfc3339()])?;
        tx.commit()?;
        Ok(changed != 0)
    }
}

#[cfg(test)]
#[path = "companion_dock_tests.rs"]
mod tests;
