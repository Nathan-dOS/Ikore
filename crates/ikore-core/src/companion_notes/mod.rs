//! Companion Notes (ADR-029): loose developer captures that the Notes Scribe
//! turns into structured notes the developer can refine by conversation.
//!
//! A capture is stored before any model call, so it survives a failed or
//! cancelled turn. The Scribe has no tools and no repository; it changes only
//! the note it was given, through a closed contract the host validates. Notes
//! follow the developer's language (ADR-029 decision 5); everything the host
//! writes stays English.
mod contract;
mod failure;
mod runtime;
mod service;
#[cfg(test)]
mod tests;

pub use contract::{NoteScribeReplyV1, reply_schema, system_prompt};
pub use failure::explain_note_failure;
pub use runtime::*;
pub use service::*;

use crate::feature_forge::TurnFailure;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Longest capture, counted after secret redaction.
pub const RAW_INPUT_MAX: usize = 8_000;
pub const REFINE_MESSAGE_MAX: usize = 2_000;
pub const TITLE_MAX: usize = 80;
pub const SUMMARY_MAX: usize = 280;
pub const CONTENT_MAX: usize = 4_000;
pub const MESSAGE_MAX: usize = 600;
pub const TAGS_MAX: usize = 3;
/// Refinement exchanges replayed to the Scribe on each refine turn.
pub const CONVERSATION_WINDOW: usize = 6;
/// Notes one snapshot carries; search and filters run over these in the UI.
pub const LIST_LIMIT: usize = 500;
/// The answer and the single corrective call a rejected answer may get.
pub const MAX_CALLS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteStatus {
    /// Captured; the Scribe has not delivered the structured text yet.
    Structuring,
    Ready,
    /// The first structuring failed or was cancelled. The capture is kept.
    Failed,
}

impl NoteStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Structuring => "structuring",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    /// Reads the stored column; its CHECK admits only these values.
    fn from_db(value: &str) -> Self {
        match value {
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            _ => Self::Structuring,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteSource {
    #[default]
    Text,
    /// Reserved for the local speech-to-text phase (ADR-029 decision 12).
    Voice,
}

impl NoteSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Voice => "voice",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "voice" => Self::Voice,
            _ => Self::Text,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteTurnKind {
    /// Turns the raw capture into the note (first attempt or a retry).
    Structure,
    /// Rewrites the note following the developer's instruction.
    Refine,
}

impl NoteTurnKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Structure => "structure",
            Self::Refine => "refine",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "refine" => Self::Refine,
            _ => Self::Structure,
        }
    }
}

/// The closed tag catalogue (ADR-029 decision 6). The model chooses ids; the
/// UI renders the host's labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteTag {
    UiUx,
    Performance,
    Bug,
    Workflow,
    Feature,
    Docs,
    Other,
}

impl NoteTag {
    pub const ALL: [Self; 7] = [
        Self::UiUx,
        Self::Performance,
        Self::Bug,
        Self::Workflow,
        Self::Feature,
        Self::Docs,
        Self::Other,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::UiUx => "ui_ux",
            Self::Performance => "performance",
            Self::Bug => "bug",
            Self::Workflow => "workflow",
            Self::Feature => "feature",
            Self::Docs => "docs",
            Self::Other => "other",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::UiUx => "UI/UX",
            Self::Performance => "Performance",
            Self::Bug => "Bug",
            Self::Workflow => "Workflow",
            Self::Feature => "Feature",
            Self::Docs => "Docs",
            Self::Other => "Other",
        }
    }

    /// What the tag means, as the Scribe reads it in the catalogue.
    pub const fn meaning(self) -> &'static str {
        match self {
            Self::UiUx => "Interface, layout, interaction or visual design",
            Self::Performance => "Speed, responsiveness or resource use",
            Self::Bug => "Something that behaves incorrectly",
            Self::Workflow => "How the developer moves through a task or between tools",
            Self::Feature => "A new capability",
            Self::Docs => "Documentation, help text or explanations",
            Self::Other => "Nothing above fits",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tag| tag.id() == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionNote {
    pub id: String,
    pub project_id: String,
    pub revision: u64,
    pub status: NoteStatus,
    pub source_type: NoteSource,
    /// The developer's text after secret redaction, verbatim otherwise.
    pub raw_input: String,
    pub raw_redactions: u32,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content_markdown: Option<String>,
    pub tags: Vec<NoteTag>,
    pub output_language: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl CompanionNote {
    /// The Scribe's title, or the start of the capture while there is none.
    pub fn display_title(&self) -> String {
        if let Some(title) = &self.title {
            return title.clone();
        }
        let first_line = self.raw_input.lines().next().unwrap_or_default().trim();
        if first_line.chars().count() <= TITLE_MAX {
            first_line.to_string()
        } else {
            first_line.chars().take(TITLE_MAX - 1).collect::<String>() + "…"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteTurn {
    pub id: String,
    pub note_id: String,
    pub note_revision: u64,
    pub ordinal: u64,
    pub kind: NoteTurnKind,
    pub status: String,
    /// The developer's instruction, for a refinement.
    pub user_message: Option<String>,
    pub user_redactions: u32,
    /// The Scribe's reply to the developer.
    pub assistant_message: Option<String>,
    pub failure: Option<TurnFailure>,
    /// What the host accepted with a reservation.
    pub notices: Vec<String>,
    pub calls: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesSnapshot {
    /// Newest first, at most [`LIST_LIMIT`].
    pub notes: Vec<CompanionNote>,
    /// Running turn id by note id.
    pub running: HashMap<String, String>,
    /// More notes exist than the snapshot carries.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureNoteRequest {
    pub project_id: String,
    /// Makes a repeated submit return the note it already created.
    pub request_id: String,
    pub raw_input: String,
    #[serde(default)]
    pub source_type: NoteSource,
}

/// A note changed. Payload-free: the UI reloads its snapshot.
#[derive(Debug, Clone)]
pub struct NotesEvent {
    pub project_id: String,
    pub note_id: Option<String>,
}
