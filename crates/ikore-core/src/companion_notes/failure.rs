//! What a failed Scribe turn means, in words a developer can act on.
//!
//! The classification is the Forge's (`TurnFailure`): the layer that knows
//! what went wrong types it once. Only the wording differs, because here the
//! subject is a note and the developer's text is always kept. Host-authored
//! English (ADR-017).
use crate::feature_forge::{FailureExplanation, TurnFailure, TurnFailureKind};

pub fn explain_note_failure(failure: &TurnFailure) -> FailureExplanation {
    let text = |summary: &str, cause: &str, remedy: &str| FailureExplanation {
        summary: summary.into(),
        cause: cause.into(),
        remedy: remedy.into(),
    };
    match failure.kind {
        TurnFailureKind::ReplyUnreadable | TurnFailureKind::ReplyLanguage => text(
            "The Notes Scribe's answer could not be used.",
            "It was cut off, was not valid JSON, or broke the note rules twice (length, plain-text title and summary, Markdown or tags), so nothing was changed. Your text is saved.",
            "Try again. If it keeps happening, choose a stronger model for the Notes Scribe in Agents.",
        ),
        TurnFailureKind::Sanitization => text(
            "The host could not sanitize this note before sending it.",
            "The call stopped before any of your text left the app. Your text is saved.",
            "Shorten the note or review the local sanitization configuration, then try again.",
        ),
        TurnFailureKind::ContextTooLarge => text(
            "The note no longer fits the Scribe's context limit.",
            "The note, its recent refinements and your message together are larger than the configured context budget, so nothing was sent.",
            "Shorten your message, or raise the context limit in Settings, then try again.",
        ),
        TurnFailureKind::ProviderSetup
            if failure.technical.contains("model_unverified")
                && failure.technical.contains("CLI default has no fixed model ID") =>
        {
            text(
                "The Notes Scribe needs an explicit CLI model.",
                "Its route uses the CLI default, which does not identify a fixed model the app can verify. Your text is saved.",
                "Open Agents, choose and save an explicit model for the Notes Scribe, then try again.",
            )
        }
        TurnFailureKind::ProviderSetup => FailureExplanation {
            summary: match &failure.subject {
                Some(subject) => format!("{subject} could not be used for this note."),
                None => "The Notes Scribe's model could not be used for this note.".into(),
            },
            cause: "The provider or CLI refused the request or is not set up: a missing or invalid key, a CLI that is not installed or signed in, or a route that points at an unavailable model. Your text is saved.".into(),
            remedy: "Open Agents, fix the Notes Scribe's route or sign in to its CLI, then try again.".into(),
        },
        TurnFailureKind::ProviderTransient => text(
            "The model could not finish this note.",
            "The provider or CLI timed out, lost the connection, hit a rate limit or stopped unexpectedly. Your text is saved and nothing was changed.",
            "Try again in a moment.",
        ),
        TurnFailureKind::StaleFeature => text(
            "The note changed while the Scribe was answering.",
            "The answer no longer matches the note, so it was not applied.",
            "Try again; the Scribe will use the current note.",
        ),
        TurnFailureKind::Interrupted => text(
            "The app was closed before the Scribe finished.",
            "The answer was still being generated when the app restarted. Your text is saved.",
            "Try again.",
        ),
        TurnFailureKind::Cancelled => text("You cancelled this response.", "", ""),
        TurnFailureKind::TooManyQuestions
        | TurnFailureKind::SourceRejected
        | TurnFailureKind::DependencyUnresolved
        | TurnFailureKind::Unclassified => text(
            "This note could not be completed.",
            "The app hit an error it has no specific explanation for. The technical detail shows what it reported. Your text is saved.",
            "Try again. If it keeps failing, share the technical detail.",
        ),
    }
}
