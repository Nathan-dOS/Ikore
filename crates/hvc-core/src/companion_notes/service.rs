//! Reading and deleting notes. Everything that starts a Scribe turn lives in
//! the runtime, which owns admission and cancellation.
use super::*;
use crate::feature_forge::service::{check_project, decode, invalid};
use crate::persistence_service::{PersistenceService, PersistenceServiceError as Error};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

#[derive(Clone)]
pub struct CompanionNotesService {
    pub(crate) persistence: PersistenceService,
}

const NOTE_COLUMNS: &str = "id,project_id,revision,status,source_type,raw_input,raw_redactions,title,summary,content_markdown,tags_json,output_language,created_at,updated_at";

impl CompanionNotesService {
    pub fn new(persistence: PersistenceService) -> Self {
        Self { persistence }
    }

    /// The project's newest notes and which of them the Scribe is working on.
    pub fn snapshot(&self, project: &str) -> Result<NotesSnapshot, Error> {
        let db = self.persistence.connection()?;
        check_project(&db, project)?;
        let mut notes = db
            .prepare(&format!(
                "SELECT {NOTE_COLUMNS} FROM companion_notes WHERE project_id=?1
                 ORDER BY created_at DESC, id DESC LIMIT ?2"
            ))?
            .query_map(params![project, LIST_LIMIT as i64 + 1], map_note)?
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = notes.len() > LIST_LIMIT;
        notes.truncate(LIST_LIMIT);
        let running = db
            .prepare(
                "SELECT t.note_id,t.id FROM companion_note_turns t
                 JOIN companion_notes n ON n.id=t.note_id
                 WHERE n.project_id=?1 AND t.status='running'",
            )?
            .query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(NotesSnapshot {
            notes,
            running,
            truncated,
        })
    }

    pub fn get(&self, project: &str, note_id: &str) -> Result<CompanionNote, Error> {
        note(&self.persistence.connection()?, project, note_id)
    }

    /// Every Scribe turn on the note, oldest first.
    pub fn history(&self, project: &str, note_id: &str) -> Result<Vec<NoteTurn>, Error> {
        let db = self.persistence.connection()?;
        note(&db, project, note_id)?;
        history(&db, note_id)
    }

    /// Deletes a note and its turns. Refused while the Scribe is working on
    /// it: cancel first, so no answer can arrive for a note that is gone.
    pub fn delete(
        &self,
        project: &str,
        note_id: &str,
        expected_revision: u64,
    ) -> Result<(), Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = note(&tx, project, note_id)?;
        if current.revision != expected_revision {
            return Err(invalid("The note changed; reload it and try again"));
        }
        if running_turn(&tx, note_id)?.is_some() {
            return Err(invalid(
                "The Notes Scribe is working on this note; cancel it before deleting",
            ));
        }
        tx.execute("DELETE FROM companion_notes WHERE id=?1", [note_id])?;
        tx.commit()?;
        Ok(())
    }
}

pub(crate) fn note(db: &Connection, project: &str, note_id: &str) -> Result<CompanionNote, Error> {
    db.query_row(
        &format!("SELECT {NOTE_COLUMNS} FROM companion_notes WHERE id=?1 AND project_id=?2"),
        params![note_id, project],
        map_note,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound("Note".into()))
}

pub(crate) fn running_turn(db: &Connection, note_id: &str) -> Result<Option<String>, Error> {
    Ok(db
        .query_row(
            "SELECT id FROM companion_note_turns WHERE note_id=?1 AND status='running'",
            [note_id],
            |r| r.get(0),
        )
        .optional()?)
}

pub(crate) fn history(db: &Connection, note_id: &str) -> Result<Vec<NoteTurn>, Error> {
    Ok(db
        .prepare(
            "SELECT id,note_id,note_revision,ordinal,kind,status,user_message,user_redactions,
                    assistant_message,failure_json,notices_json,calls,created_at
             FROM companion_note_turns WHERE note_id=?1 ORDER BY ordinal",
        )?
        .query_map([note_id], |r| {
            let failure: Option<String> = r.get(9)?;
            let notices: Option<String> = r.get(10)?;
            Ok(NoteTurn {
                id: r.get(0)?,
                note_id: r.get(1)?,
                note_revision: r.get(2)?,
                ordinal: r.get(3)?,
                kind: NoteTurnKind::from_db(&r.get::<_, String>(4)?),
                status: r.get(5)?,
                user_message: r.get(6)?,
                user_redactions: r.get(7)?,
                assistant_message: r.get(8)?,
                failure: failure.and_then(|raw| serde_json::from_str(&raw).ok()),
                notices: notices
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_default(),
                calls: r.get(11)?,
                created_at: r.get(12)?,
            })
        })?
        .collect::<Result<_, _>>()?)
}

fn map_note(r: &Row<'_>) -> rusqlite::Result<CompanionNote> {
    let tags: String = r.get(10)?;
    Ok(CompanionNote {
        id: r.get(0)?,
        project_id: r.get(1)?,
        revision: r.get(2)?,
        status: NoteStatus::from_db(&r.get::<_, String>(3)?),
        source_type: NoteSource::from_db(&r.get::<_, String>(4)?),
        raw_input: r.get(5)?,
        raw_redactions: r.get(6)?,
        title: r.get(7)?,
        summary: r.get(8)?,
        content_markdown: r.get(9)?,
        // Stored by the host from validated ids; an unknown id is dropped
        // rather than failing the whole list.
        tags: decode::<Vec<String>>(&tags)
            .unwrap_or_default()
            .iter()
            .filter_map(|id| NoteTag::parse(id))
            .collect(),
        output_language: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}
