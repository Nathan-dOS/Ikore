//! Resuming a CLI session on another model (PI-Desktop analysis, PD10).
//!
//! A CLI's native session keeps the backend and model it started with, so a
//! resume after the developer picked others starts a new session instead. It
//! opens with what the host kept of the earlier one, the agent's last answers,
//! labelled as data (ADR-037), and then the developer's message. The host never
//! kept the developer's earlier messages, so only the agent's side comes along.

use std::fmt::Write as _;

use ikore_contracts::exchange::{
    AgentProducer, ExchangeItemV1, ExchangeKind, HostComponent, HumanSource, Producer, fence_for,
};

use super::{
    CliBackend, TerminalError, TerminalEvent, TerminalEventKind, TerminalRun, TerminalService,
};

/// Runs read back through their parents, at most: a guard, not a budget.
const MAX_SESSION_RUNS: usize = 1_000;
/// Turns quoted, at most: the newest ones.
const MAX_QUOTED_TURNS: usize = 6;
/// Characters kept of one answer, from its end, where the agent says where it got to.
const MAX_ANSWER_CHARS: usize = 2_000;
/// Characters of answers quoted in all; the oldest turns are left out first.
const MAX_QUOTED_CHARS: usize = 8_000;

/// Whether resuming `source` on `backend` and `model` needs a new session.
pub fn needs_handoff(source: &TerminalRun, backend: CliBackend, model: Option<&str>) -> bool {
    source.backend != Some(backend) || source.model.as_deref() != model
}

/// "Codex · gpt-5", or the CLI's own default when no model was chosen.
pub(super) fn target_label(backend: Option<CliBackend>, model: Option<&str>) -> String {
    let backend = backend.map_or("an unknown CLI", CliBackend::label);
    let model = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or("CLI default");
    format!("{backend} \u{b7} {model}")
}

/// One turn of the earlier session, as the new one reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct QuotedTurn {
    /// Counted from the session's first turn.
    number: usize,
    status: &'static str,
    answer: Option<String>,
}

/// What the agent said last in a run: its last text message or result. Rows the
/// host wrote, which have no stream ("Run completed."), and rows that only say a
/// protocol frame arrived are not answers.
fn last_answer(events: &[TerminalEvent]) -> Option<&str> {
    events
        .iter()
        .rev()
        .filter(|event| {
            event.stream.is_some()
                && matches!(
                    event.kind,
                    TerminalEventKind::Message | TerminalEventKind::Completed
                )
        })
        .map(|event| event.content.trim())
        .find(|content| {
            !content.is_empty() && !(content.starts_with("CLI ") && content.contains(" received"))
        })
}

/// The end of `text`, at most `limit` characters, marked when cut.
fn tail(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().skip(count - limit).collect();
    format!("[…]{kept}")
}

/// The text a new session opens with, up to where the developer's message
/// starts. `turns` go oldest first.
fn render_handoff(from: &str, to: &str, total: &str, turns: &[QuotedTurn]) -> String {
    let host = ExchangeItemV1::new(
        ExchangeKind::DerivedContext,
        Producer::Host(HostComponent::TerminalResume),
    );
    let agent = ExchangeItemV1::new(
        ExchangeKind::ConversationMessage,
        Producer::agent(AgentProducer::TerminalSession),
    );
    let developer = ExchangeItemV1::new(
        ExchangeKind::ConversationMessage,
        Producer::Human(HumanSource::CliPrompt),
    );
    let mut text = String::from("## Handoff from an earlier session\n");
    let _ = writeln!(text, "{}", host.provenance_line());
    let _ = writeln!(
        text,
        "The developer moved this work from {from} to {to}. A CLI session keeps the model it \
         started with, so this is a new session. Below are the agent's last answers in the \
         earlier one, oldest first: {} of its {total} turns. The developer's messages there \
         were not kept. Files may have changed since; read them before relying on an answer.",
        turns.len()
    );
    for turn in turns {
        let _ = writeln!(text, "\n### Turn {} \u{b7} {}", turn.number, turn.status);
        match &turn.answer {
            Some(answer) => {
                let fence = fence_for(answer);
                let _ = writeln!(text, "{}", agent.provenance_line());
                let _ = writeln!(text, "{fence}\n{answer}\n{fence}");
            }
            None => text.push_str("No answer was recorded.\n"),
        }
    }
    text.push_str("\n## The developer's message\n");
    let _ = writeln!(text, "{}", developer.provenance_line());
    text
}

impl TerminalService {
    /// What a new session on `backend` and `model` is told about the session
    /// `source` belongs to: where it ran and the agent's last answers there.
    pub(super) fn handoff_notice(
        &self,
        source: &TerminalRun,
        backend: CliBackend,
        model: Option<&str>,
    ) -> Result<String, TerminalError> {
        // The session's runs, newest first: each resume names the run it continued.
        let mut runs = vec![source.clone()];
        while runs.len() < MAX_SESSION_RUNS {
            let Some(parent) = runs.last().and_then(|run| run.parent_run_id.clone()) else {
                break;
            };
            runs.push(self.repository.get_run(&parent)?);
        }
        let total = if runs.last().is_some_and(|run| run.parent_run_id.is_some()) {
            format!("more than {}", runs.len())
        } else {
            runs.len().to_string()
        };
        let mut budget = MAX_QUOTED_CHARS;
        let mut turns = Vec::new();
        for (index, run) in runs.iter().enumerate().take(MAX_QUOTED_TURNS) {
            let events = match self.repository.redacted_run_events(&run.id) {
                Ok(events) => events,
                // Too long to read back whole: its turn shows without an answer.
                Err(TerminalError::OutputLimit) => Vec::new(),
                Err(error) => return Err(error),
            };
            let answer = last_answer(&events).map(|answer| tail(answer, MAX_ANSWER_CHARS));
            let size = answer.as_deref().map_or(0, |answer| answer.chars().count());
            if size > budget {
                break;
            }
            budget -= size;
            turns.push(QuotedTurn {
                number: runs.len() - index,
                status: run.status.as_str(),
                answer,
            });
        }
        turns.reverse();
        Ok(render_handoff(
            &target_label(source.backend, source.model.as_deref()),
            &target_label(Some(backend), model),
            &total,
            &turns,
        ))
    }
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
