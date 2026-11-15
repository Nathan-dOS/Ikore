//! Moving through the Companion's conversations (PI-Desktop analysis, PD11): branch one from a
//! message, search them, and pin or archive them. Pins and archives live per project in
//! `runtime_settings`, as the remembered Companion target does, so no table changes.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{
    ChatCommandError, ChatService, GetChatConversationRequest, GetChatConversationResponse, Uuid,
    normalize_id, now_iso,
};

/// Matches kept per search: the list is for picking a conversation, not for reading them all.
pub const MAX_CONVERSATION_MATCHES: usize = 50;
/// Characters of a message shown around a match.
const EXCERPT_CHARS: usize = 120;
/// Characters of context kept before the match.
const EXCERPT_LEAD: usize = 40;

#[derive(Debug, Clone)]
pub struct ForkChatConversationRequest {
    pub conversation_id: String,
    /// The last message the branch keeps.
    pub through_message_id: String,
}

#[derive(Debug, Clone)]
pub struct SearchChatConversationsRequest {
    pub project_id: String,
    pub query: String,
}

/// A conversation whose title or messages hold the query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatConversationMatch {
    pub conversation_id: String,
    pub title: String,
    /// The first message that matches, cut around the match. `None` when only the title does.
    pub excerpt: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationMark {
    Pinned,
    Archived,
}

#[derive(Debug, Clone)]
pub struct MarkChatConversationRequest {
    pub conversation_id: String,
    pub mark: ConversationMark,
    pub on: bool,
}

/// The pinned and archived conversations of a project. A conversation is never both: pinning
/// one takes it out of the archive, and archiving one unpins it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationMarks {
    #[serde(default)]
    pub pinned: Vec<String>,
    #[serde(default)]
    pub archived: Vec<String>,
}

impl ConversationMarks {
    pub fn has(&self, mark: ConversationMark, conversation_id: &str) -> bool {
        self.list(mark).iter().any(|id| id == conversation_id)
    }

    fn list(&self, mark: ConversationMark) -> &Vec<String> {
        match mark {
            ConversationMark::Pinned => &self.pinned,
            ConversationMark::Archived => &self.archived,
        }
    }

    fn set(&mut self, mark: ConversationMark, conversation_id: &str, on: bool) {
        self.pinned.retain(|id| id != conversation_id);
        self.archived.retain(|id| id != conversation_id);
        if on {
            match mark {
                // The newest pin goes first.
                ConversationMark::Pinned => self.pinned.insert(0, conversation_id.to_owned()),
                ConversationMark::Archived => self.archived.push(conversation_id.to_owned()),
            }
        }
    }
}

fn marks_key(project_id: &str) -> String {
    format!("companion.marks.{project_id}")
}

/// The marks of a project, keeping only conversations that still exist there.
pub(crate) fn read_marks(
    connection: &Connection,
    project_id: &str,
) -> rusqlite::Result<ConversationMarks> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT value FROM runtime_settings WHERE key = ?1",
            [marks_key(project_id)],
            |row| row.get(0),
        )
        .optional()?;
    let mut marks: ConversationMarks = stored
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    if marks.pinned.is_empty() && marks.archived.is_empty() {
        return Ok(marks);
    }
    let mut statement =
        connection.prepare("SELECT id FROM chat_conversations WHERE project_id = ?1")?;
    let existing = statement
        .query_map([project_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    marks.pinned.retain(|id| existing.contains(id));
    marks.archived.retain(|id| existing.contains(id));
    Ok(marks)
}

/// `%` and `_` are wildcards of LIKE; the query matches them literally.
fn like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for character in query.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

/// A one-line window of `text` around the first case-insensitive match of `query`.
fn excerpt(text: &str, query: &str) -> String {
    let fold = |character: char| character.to_lowercase().next().unwrap_or(character);
    let characters: Vec<char> = text.chars().collect();
    let needle: Vec<char> = query.chars().map(fold).collect();
    let found = (!needle.is_empty() && needle.len() <= characters.len())
        .then(|| {
            (0..=characters.len() - needle.len()).find(|&start| {
                characters[start..start + needle.len()]
                    .iter()
                    .zip(&needle)
                    .all(|(character, wanted)| fold(*character) == *wanted)
            })
        })
        .flatten()
        .unwrap_or(0);
    let start = found.saturating_sub(EXCERPT_LEAD);
    let end = (start + EXCERPT_CHARS).min(characters.len());
    let mut window: String = characters[start..end]
        .iter()
        .map(|character| {
            if character.is_whitespace() {
                ' '
            } else {
                *character
            }
        })
        .collect();
    window = window.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        window.insert(0, '…');
    }
    if end < characters.len() {
        window.push('…');
    }
    window
}

impl ChatService {
    /// A new conversation with the same target and the messages up to `through_message_id`.
    /// The Companion sends the history with every turn, so the branch goes on from there on
    /// either transport. The original's tool activity, model calls and token usage stay with it.
    pub fn fork_conversation(
        &self,
        request: ForkChatConversationRequest,
    ) -> Result<GetChatConversationResponse, ChatCommandError> {
        let conversation_id = normalize_id(&request.conversation_id, "conversationId")?;
        let message_id = normalize_id(&request.through_message_id, "messageId")?;
        let database = self.connection()?;
        let transaction = database.unchecked_transaction()?;
        let through: Option<(i64, String)> = transaction
            .query_row(
                "SELECT ordinal, status FROM chat_messages WHERE id = ?1 AND conversation_id = ?2",
                params![message_id, conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((through, status)) = through else {
            return Err(ChatCommandError::new(
                "message_not_found",
                "That message is not in this conversation.",
            ));
        };
        if matches!(status.as_str(), "pending" | "streaming") {
            return Err(ChatCommandError::new(
                "turn_active",
                "Wait for the reply to end before branching from it.",
            ));
        }
        let fork_id = Uuid::new_v4().to_string();
        let now = now_iso();
        let created = transaction.execute(
            "INSERT INTO chat_conversations (
               id, project_id, connection_id, provider_id_snapshot, model_id_snapshot, title,
               created_at, updated_at, transport_kind, cli_backend, model_profile, model_source
             )
             SELECT ?1, project_id, connection_id, provider_id_snapshot, model_id_snapshot,
                    title || ' · branch', ?2, ?2, transport_kind, cli_backend, model_profile,
                    model_source
             FROM chat_conversations WHERE id = ?3",
            params![fork_id, now, conversation_id],
        )?;
        if created == 0 {
            return Err(ChatCommandError::new(
                "conversation_not_found",
                "Conversation not found.",
            ));
        }
        let messages = {
            let mut statement = transaction.prepare(
                "SELECT ordinal, role, content, status, error_code, error_message, created_at,
                        target_label
                 FROM chat_messages
                 WHERE conversation_id = ?1 AND ordinal <= ?2
                   AND status NOT IN ('pending', 'streaming')
                 ORDER BY ordinal",
            )?;
            statement
                .query_map(params![conversation_id, through], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (ordinal, role, content, status, error_code, error_message, created_at, target_label) in
            messages
        {
            transaction.execute(
                "INSERT INTO chat_messages (
                   id, conversation_id, ordinal, role, content, status, error_code,
                   error_message, created_at, updated_at, target_label
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    Uuid::new_v4().to_string(),
                    fork_id,
                    ordinal,
                    role,
                    content,
                    status,
                    error_code,
                    error_message,
                    created_at,
                    now,
                    target_label,
                ],
            )?;
        }
        transaction.commit()?;
        self.get_conversation(GetChatConversationRequest {
            conversation_id: fork_id,
        })
    }

    /// The project's conversations whose title or messages hold `query`, ignoring ASCII case,
    /// the most recent first.
    pub fn search_conversations(
        &self,
        request: SearchChatConversationsRequest,
    ) -> Result<Vec<ChatConversationMatch>, ChatCommandError> {
        let project_id = normalize_id(&request.project_id, "projectId")?;
        let query = request.query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let database = self.connection()?;
        let mut statement = database.prepare(
            "SELECT c.id, c.title, c.updated_at,
                    (SELECT m.content FROM chat_messages m
                     WHERE m.conversation_id = c.id AND m.content LIKE ?2 ESCAPE '\\'
                     ORDER BY m.ordinal LIMIT 1)
             FROM chat_conversations c
             WHERE c.project_id = ?1
               AND (c.title LIKE ?2 ESCAPE '\\'
                    OR EXISTS(SELECT 1 FROM chat_messages m
                              WHERE m.conversation_id = c.id AND m.content LIKE ?2 ESCAPE '\\'))
             ORDER BY c.updated_at DESC, c.id
             LIMIT ?3",
        )?;
        let limit = i64::try_from(MAX_CONVERSATION_MATCHES).unwrap_or(i64::MAX);
        let matches = statement
            .query_map(params![project_id, like_pattern(query), limit], |row| {
                Ok(ChatConversationMatch {
                    conversation_id: row.get(0)?,
                    title: row.get(1)?,
                    updated_at: row.get(2)?,
                    excerpt: row
                        .get::<_, Option<String>>(3)?
                        .map(|content| excerpt(&content, query)),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(matches)
    }

    pub fn conversation_marks(
        &self,
        project_id: &str,
    ) -> Result<ConversationMarks, ChatCommandError> {
        let project_id = normalize_id(project_id, "projectId")?;
        Ok(read_marks(&self.connection()?, &project_id)?)
    }

    /// Pins, unpins, archives or restores a conversation, and returns its project's marks.
    pub fn mark_conversation(
        &self,
        request: MarkChatConversationRequest,
    ) -> Result<ConversationMarks, ChatCommandError> {
        let conversation_id = normalize_id(&request.conversation_id, "conversationId")?;
        let database = self.connection()?;
        let transaction = database.unchecked_transaction()?;
        let project_id: Option<String> = transaction
            .query_row(
                "SELECT project_id FROM chat_conversations WHERE id = ?1",
                [&conversation_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(project_id) = project_id else {
            return Err(ChatCommandError::new(
                "conversation_not_found",
                "Conversation not found.",
            ));
        };
        let mut marks = read_marks(&transaction, &project_id)?;
        marks.set(request.mark, &conversation_id, request.on);
        let value = serde_json::to_string(&marks).map_err(|error| {
            ChatCommandError::new(
                "invalid_marks",
                format!("Marks could not be saved: {error}"),
            )
        })?;
        transaction.execute(
            "INSERT INTO runtime_settings(key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![marks_key(&project_id), value, now_iso()],
        )?;
        transaction.commit()?;
        Ok(marks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_and_archive_exclude_each_other_and_the_newest_pin_comes_first() {
        let mut marks = ConversationMarks::default();
        marks.set(ConversationMark::Pinned, "a", true);
        marks.set(ConversationMark::Pinned, "b", true);
        assert_eq!(marks.pinned, ["b", "a"]);
        marks.set(ConversationMark::Archived, "a", true);
        assert_eq!(marks.pinned, ["b"]);
        assert_eq!(marks.archived, ["a"]);
        marks.set(ConversationMark::Pinned, "a", true);
        assert!(marks.archived.is_empty());
        assert!(marks.has(ConversationMark::Pinned, "a"));
        marks.set(ConversationMark::Pinned, "a", false);
        assert!(!marks.has(ConversationMark::Pinned, "a"));
        assert_eq!(marks.pinned, ["b"]);
    }

    #[test]
    fn excerpts_stay_on_one_line_around_the_match() {
        let text = format!(
            "{}\nThe NEEDLE is here\n{}",
            "a ".repeat(60),
            "b ".repeat(60)
        );
        let cut = excerpt(&text, "needle");
        assert!(cut.starts_with('…') && cut.ends_with('…'), "{cut}");
        assert!(cut.contains("The NEEDLE is here"), "{cut}");
        assert!(!cut.contains('\n'));
        assert_eq!(excerpt("short text", "missing"), "short text");
        assert_eq!(like_pattern("50%_a\\"), "%50\\%\\_a\\\\%");
    }
}
