//! The end-of-pulsation HTML report (ADR-027 §5).
//!
//! [`render_pulsation_report`] is a pure function of a typed
//! [`PulsationReport`]: it reads no clock, no file and no database, so the same
//! input always yields the same bytes. Callers build the input from data the
//! host already validated; the report is a derived view, never a document of
//! record.

use serde::{Deserialize, Serialize};

use super::html::{SafeHtml, document, element};
use crate::orchestration::PublicationState;
use crate::orchestration::catalog::STAGES;
use crate::orchestration::composition::{CheckStatus, Verdict};
use crate::persistence_service::{PulsationStage, PulsationStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PulsationReport {
    pub pulsation: ReportPulsation,
    /// Criteria, checks and files. `None` when the report was built from run
    /// telemetry alone; the renderer then says so instead of showing empty
    /// sections.
    pub outcome: Option<ReportOutcome>,
    pub run: ReportRun,
    pub rejected_outputs: Vec<RejectedOutput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportPulsation {
    pub id: String,
    pub feature_name: String,
    pub status: PulsationStatus,
    pub stage: PulsationStage,
    pub pipeline_version: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportOutcome {
    pub criteria: Vec<ReportCriterion>,
    /// Agent-written blockers from the Integration payload.
    pub blockers: Vec<String>,
    pub checks: Vec<ReportCheck>,
    pub files: Vec<ReportFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportCriterion {
    /// Developer-facing id, such as `AC-1`.
    pub criterion_id: String,
    pub description: String,
    pub task_title: String,
    pub verdict: Verdict,
    /// Agent-written explanation of the verdict.
    pub explanation: String,
    pub check_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportCheck {
    pub check_ref: String,
    pub required: bool,
    pub status: CheckStatus,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_seconds: Option<f64>,
    pub receipt_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportFile {
    pub path: String,
    pub change: FileChange,
    pub lines_added: u32,
    pub lines_removed: u32,
    pub publication: PublicationState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportRun {
    pub wall_clock_seconds: f64,
    /// Time agents spent running inside the six stages.
    pub agent_seconds: f64,
    /// Time parked at checkpoints that asked for a decision about the work.
    pub review_wait_seconds: f64,
    /// Time parked at checkpoints that only asked to unblock a failure.
    pub unblock_wait_seconds: f64,
    pub tokens: ReportTokens,
    pub stages: Vec<ReportStage>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportTokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub discarded_input: i64,
    pub discarded_output: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportStage {
    pub stage: PulsationStage,
    pub runs: i64,
    pub agent_seconds: f64,
    pub discarded_seconds: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectedOutput {
    pub agent: String,
    pub raw_output_bytes: i64,
    pub error: String,
}

/// Renders the complete, self-contained report document.
pub fn render_pulsation_report(report: &PulsationReport) -> String {
    let pulsation = &report.pulsation;
    let mut page = SafeHtml::markup(SPRITE);
    page.push_markup("<div class=\"page\">")
        .push(&header(pulsation))
        .push(&navigation(report))
        .push_markup("<main>");
    match &report.outcome {
        Some(outcome) => {
            page.push(&summary(outcome))
                .push(&criteria(outcome))
                .push(&not_verified(outcome))
                .push(&checks(&outcome.checks))
                .push(&files(&outcome.files));
        }
        None => {
            page.push(&outcome_unavailable());
        }
    }
    page.push(&run_context(&report.run));
    if !report.rejected_outputs.is_empty() {
        page.push(&rejected_outputs(&report.rejected_outputs));
    }
    page.push_markup("</main>")
        .push(&footer())
        .push_markup("</div>");
    document(
        &format!("Pulsation report · {}", pulsation.feature_name),
        &format!(
            "Host-rendered report of pulsation {} ({}).",
            pulsation.id, pulsation.feature_name
        ),
        &page,
    )
}

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Good,
    Warn,
    Crit,
    Neutral,
}

impl Tone {
    const fn class(self) -> &'static str {
        match self {
            Self::Good => "is-good",
            Self::Warn => "is-warn",
            Self::Crit => "is-crit",
            Self::Neutral => "is-neutral",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Icon {
    Check,
    Cross,
    Help,
    Alert,
    Minus,
    File,
    Info,
}

impl Icon {
    const fn id(self) -> &'static str {
        match self {
            Self::Check => "i-check",
            Self::Cross => "i-x",
            Self::Help => "i-help",
            Self::Alert => "i-alert",
            Self::Minus => "i-minus",
            Self::File => "i-file",
            Self::Info => "i-info",
        }
    }
}

const SPRITE: &str = concat!(
    "<svg width=\"0\" height=\"0\" style=\"position:absolute\" aria-hidden=\"true\" focusable=\"false\">",
    "<symbol id=\"i-check\" viewBox=\"0 0 24 24\"><path d=\"M20 6 9 17l-5-5\"/></symbol>",
    "<symbol id=\"i-x\" viewBox=\"0 0 24 24\"><path d=\"M18 6 6 18\"/><path d=\"m6 6 12 12\"/></symbol>",
    "<symbol id=\"i-help\" viewBox=\"0 0 24 24\"><circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3\"/><path d=\"M12 17h.01\"/></symbol>",
    "<symbol id=\"i-alert\" viewBox=\"0 0 24 24\"><path d=\"m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3\"/><path d=\"M12 9v4\"/><path d=\"M12 17h.01\"/></symbol>",
    "<symbol id=\"i-minus\" viewBox=\"0 0 24 24\"><circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M8 12h8\"/></symbol>",
    "<symbol id=\"i-file\" viewBox=\"0 0 24 24\"><path d=\"M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z\"/><path d=\"M14 2v4a2 2 0 0 0 2 2h4\"/></symbol>",
    "<symbol id=\"i-info\" viewBox=\"0 0 24 24\"><circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M12 16v-4\"/><path d=\"M12 8h.01\"/></symbol>",
    "</svg>"
);

fn icon(icon: Icon) -> SafeHtml {
    let mut html = SafeHtml::markup("<svg class=\"ic\" aria-hidden=\"true\"><use href=\"#");
    html.push_markup(icon.id()).push_markup("\"/></svg>");
    html
}

/// An inline label whose icon carries the tone: `<span class="status is-x">`.
fn status_label(tone: Tone, glyph: Icon, label: &'static str) -> SafeHtml {
    let mut html = SafeHtml::markup("<span class=\"status ");
    html.push_markup(tone.class())
        .push_markup("\">")
        .push(&icon(glyph))
        .push_markup(label)
        .push_markup("</span>");
    html
}

const fn verdict_look(verdict: Verdict) -> (Tone, Icon, &'static str) {
    match verdict {
        Verdict::Supported => (Tone::Good, Icon::Check, "Supported"),
        Verdict::Unverified => (Tone::Warn, Icon::Help, "Unverified"),
        Verdict::Contradicted => (Tone::Crit, Icon::Cross, "Contradicted"),
    }
}

const fn check_look(status: CheckStatus) -> (Tone, Icon, &'static str) {
    match status {
        CheckStatus::Passed => (Tone::Good, Icon::Check, "Passed"),
        CheckStatus::Failed => (Tone::Crit, Icon::Cross, "Failed"),
        CheckStatus::TimedOut => (Tone::Crit, Icon::Cross, "Timed out"),
        CheckStatus::Error => (Tone::Crit, Icon::Cross, "Error"),
        CheckStatus::NotRun => (Tone::Neutral, Icon::Minus, "Not run"),
        CheckStatus::Unavailable => (Tone::Neutral, Icon::Minus, "Unavailable"),
        CheckStatus::Cancelled => (Tone::Neutral, Icon::Minus, "Cancelled"),
        CheckStatus::Pending => (Tone::Neutral, Icon::Minus, "Pending"),
        CheckStatus::Running => (Tone::Neutral, Icon::Minus, "Running"),
    }
}

const fn publication_look(state: PublicationState) -> (Tone, Icon, &'static str) {
    match state {
        PublicationState::NotRequired => (Tone::Neutral, Icon::Minus, "Not required"),
        PublicationState::Pending => (Tone::Neutral, Icon::Minus, "Pending approval"),
        PublicationState::Applying => (Tone::Neutral, Icon::Minus, "Applying"),
        PublicationState::Applied => (Tone::Good, Icon::Check, "Applied"),
        PublicationState::Conflict => (Tone::Crit, Icon::Alert, "Conflict"),
    }
}

const fn change_label(change: FileChange) -> &'static str {
    match change {
        FileChange::Added => "New",
        FileChange::Modified => "Modified",
        FileChange::Deleted => "Deleted",
    }
}

const fn status_look(status: PulsationStatus) -> (Tone, &'static str) {
    match status {
        PulsationStatus::Idle => (Tone::Neutral, "Idle"),
        PulsationStatus::Planning => (Tone::Neutral, "Planning"),
        PulsationStatus::Running => (Tone::Neutral, "Running"),
        PulsationStatus::Checkpoint => (Tone::Warn, "Waiting for a decision"),
        PulsationStatus::Done => (Tone::Good, "Done"),
        PulsationStatus::Failed => (Tone::Crit, "Failed"),
    }
}

const fn stage_label(stage: PulsationStage) -> &'static str {
    match stage {
        PulsationStage::Intake => "Intake",
        PulsationStage::Specification => "Specification",
        PulsationStage::Contract => "Contract",
        PulsationStage::Code => "Code",
        PulsationStage::Test => "Test",
        PulsationStage::Integrator => "Integrator",
    }
}

fn check_passed(outcome: &ReportOutcome, check_ref: &str) -> Option<CheckStatus> {
    outcome
        .checks
        .iter()
        .find(|check| check.check_ref == check_ref)
        .map(|check| check.status)
}

fn backed_by_passing_check(outcome: &ReportOutcome, criterion: &ReportCriterion) -> bool {
    criterion
        .check_refs
        .iter()
        .any(|check_ref| check_passed(outcome, check_ref) == Some(CheckStatus::Passed))
}

// ---------------------------------------------------------------------------
// Number formatting (deterministic, no locale)
// ---------------------------------------------------------------------------

fn duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as i64;
    let (hours, minutes, rest) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours} h {minutes:02} min")
    } else if minutes > 0 {
        format!("{minutes} min {rest:02} s")
    } else {
        format!("{rest} s")
    }
}

fn check_duration(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("{:.1} s", seconds.max(0.0))
    } else {
        duration(seconds)
    }
}

fn compact(value: f64) -> String {
    let magnitude = value.abs();
    let (scaled, suffix) = if magnitude >= 1_000_000.0 {
        (value / 1_000_000.0, "M")
    } else if magnitude >= 1_000.0 {
        (value / 1_000.0, "K")
    } else {
        return format!("{value:.0}");
    };
    let text = format!("{scaled:.1}");
    format!("{}{suffix}", text.strip_suffix(".0").unwrap_or(&text))
}

fn share(part: f64, whole: f64) -> f64 {
    if whole > 0.0 {
        (part / whole * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// The smallest 1/2/2.5/5 × 10ⁿ at or above `value`, so axis ticks at
/// quarters land on round numbers.
fn nice_ceiling(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    // `powi` multiplies exactly for these small exponents; `powf` may not.
    let exponent = 10f64.powi(value.log10().floor() as i32);
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|step| step * exponent)
        .find(|candidate| value <= *candidate)
        .unwrap_or(10.0 * exponent)
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn header(pulsation: &ReportPulsation) -> SafeHtml {
    let (tone, status) = status_look(pulsation.status);
    let mut html = SafeHtml::markup(
        "<header><div class=\"top\"><div class=\"brand\"><div class=\"mark\" aria-hidden=\"true\">HV</div><div><b>Hard Vibe Code</b><span>Pulsation report</span></div></div><div class=\"ids\"><span>Pulsation <code",
    );
    html.push_attr("title", &pulsation.id)
        .push_markup(">")
        .push_text(short_id(&pulsation.id))
        .push_markup("</code></span><span>Pipeline <code>v")
        .push_text(&pulsation.pipeline_version.to_string())
        .push_markup("</code></span></div></div><span class=\"eyebrow\"><span class=\"dot ")
        .push_markup(tone.class())
        .push_markup("\" aria-hidden=\"true\"></span>")
        .push_markup(stage_label(pulsation.stage))
        .push_markup(" · ")
        .push_markup(status)
        .push_markup("</span><h1>")
        .push_text(&pulsation.feature_name)
        .push_markup("</h1><div class=\"meta\"><span>Started ")
        .push_text(&pulsation.created_at)
        .push_markup("</span><span>Last update ")
        .push_text(&pulsation.updated_at)
        .push_markup("</span></div>")
        .push(&pipeline(pulsation.stage, pulsation.status))
        .push_markup("</header>");
    html
}

fn pipeline(stage: PulsationStage, status: PulsationStatus) -> SafeHtml {
    let current = STAGES
        .iter()
        .position(|(candidate, _, _)| *candidate == stage)
        .unwrap_or(0);
    let mut html = SafeHtml::markup("<ol class=\"steps\" aria-label=\"Pipeline\">");
    for (index, (candidate, _, _)) in STAGES.iter().enumerate() {
        let label = stage_label(*candidate);
        if status == PulsationStatus::Done || index < current {
            html.push_markup("<li class=\"done\">")
                .push(&icon(Icon::Check));
        } else if index == current && status == PulsationStatus::Failed {
            html.push_markup("<li class=\"failed\" aria-current=\"step\">")
                .push(&icon(Icon::Cross));
        } else if index == current {
            html.push_markup(
                "<li class=\"current\" aria-current=\"step\"><span class=\"ring\" aria-hidden=\"true\"></span>",
            );
        } else {
            html.push_markup(
                "<li class=\"pending\"><span class=\"ring\" aria-hidden=\"true\"></span>",
            );
        }
        html.push_markup("<span>")
            .push_markup(label)
            .push_markup("</span></li>");
    }
    html.push_markup("</ol>");
    html
}

fn navigation(report: &PulsationReport) -> SafeHtml {
    let mut html = SafeHtml::markup("<nav class=\"toc\" aria-label=\"Sections\"><ol>");
    if report.outcome.is_some() {
        html.push_markup(
            "<li><a href=\"#summary\">Summary</a></li><li><a href=\"#criteria\">Criteria</a></li><li><a href=\"#not-verified\">Not verified</a></li><li><a href=\"#checks\">Host checks</a></li><li><a href=\"#files\">Files</a></li>",
        );
    }
    html.push_markup("<li><a href=\"#run\">Run context</a></li>");
    if !report.rejected_outputs.is_empty() {
        html.push_markup("<li><a href=\"#rejected\">Rejected output</a></li>");
    }
    html.push_markup("</ol></nav>");
    html
}

fn outcome_unavailable() -> SafeHtml {
    let mut html = SafeHtml::markup("<section id=\"outcome\"><div class=\"card notice\">");
    html.push(&icon(Icon::Info)).push_markup(
        "<p>This report was built from run telemetry only. Criteria verdicts, host checks and files are not included.</p></div></section>",
    );
    html
}

fn summary(outcome: &ReportOutcome) -> SafeHtml {
    let count = |verdict: Verdict| {
        outcome
            .criteria
            .iter()
            .filter(|criterion| criterion.verdict == verdict)
            .count()
    };
    let supported = count(Verdict::Supported);
    let unverified = count(Verdict::Unverified);
    let contradicted = count(Verdict::Contradicted);
    let backed = outcome
        .criteria
        .iter()
        .filter(|criterion| {
            criterion.verdict == Verdict::Supported && backed_by_passing_check(outcome, criterion)
        })
        .count();

    let mut html = SafeHtml::markup(
        "<section id=\"summary\" aria-labelledby=\"summary-h\"><h2 id=\"summary-h\" class=\"sr\">Summary</h2><div class=\"summary\"><div class=\"card\"><div class=\"hero-label\">Criteria supported by the Integrator</div><div class=\"hero\"><span class=\"num\">",
    );
    html.push_text(&supported.to_string())
        .push_markup("</span><span class=\"of\">of ")
        .push_text(&outcome.criteria.len().to_string())
        .push_markup("</span></div><p class=\"hero-sub\">")
        .push_text(&format!(
            "{contradicted} contradicted and {unverified} unverified. {backed} of {supported} supported criteria are backed by a passing host check."
        ))
        .push_markup("</p><div class=\"strip\" aria-label=\"Verdict per criterion\">");
    for (index, criterion) in outcome.criteria.iter().enumerate() {
        let (tone, glyph, label) = verdict_look(criterion.verdict);
        html.push_markup("<a class=\"cell ")
            .push_markup(tone.class())
            .push_markup("\"")
            .push_attr("href", &format!("#c-{}", index + 1))
            .push_attr(
                "title",
                &format!(
                    "{} · {label} · {}",
                    criterion.criterion_id, criterion.description
                ),
            )
            .push_markup("><b>")
            .push(&icon(glyph))
            .push_text(&criterion.criterion_id)
            .push_markup("</b><span>")
            .push_markup(label)
            .push_markup("</span></a>");
    }
    html.push_markup("</div><div class=\"legend\">");
    for (verdict, total) in [
        (Verdict::Supported, supported),
        (Verdict::Unverified, unverified),
        (Verdict::Contradicted, contradicted),
    ] {
        let (tone, glyph, label) = verdict_look(verdict);
        html.push_markup("<span class=\"")
            .push_markup(tone.class())
            .push_markup("\">")
            .push(&icon(glyph))
            .push_markup(label)
            .push_markup(" ")
            .push_text(&total.to_string())
            .push_markup("</span>");
    }
    html.push_markup("</div></div>")
        .push(&tiles(outcome))
        .push_markup("</div>");
    if !outcome.blockers.is_empty() {
        html.push_markup("<div class=\"callout\" role=\"note\">")
            .push(&icon(Icon::Alert))
            .push_markup("<h3>")
            .push_text(&plural(outcome.blockers.len(), "blocker", "blockers"))
            .push_markup("</h3><ul>");
        for blocker in &outcome.blockers {
            html.push(&element("li", "", &SafeHtml::text(blocker)));
        }
        html.push_markup(
            "</ul><span class=\"src\">Agent-written blockers from the Integration payload. The host validated their shape, not their claims.</span></div>",
        );
    }
    html.push_markup("</section>");
    html
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn tile(tone: Tone, glyph: Icon, label: &'static str, value: &str, sub: &str) -> SafeHtml {
    let mut html = SafeHtml::markup("<div class=\"tile ");
    html.push_markup(tone.class())
        .push_markup("\"><span class=\"label\">")
        .push(&icon(glyph))
        .push_markup(label)
        .push_markup("</span><span class=\"value\">")
        .push_text(value)
        .push_markup("</span><span class=\"sub\">")
        .push_text(sub)
        .push_markup("</span></div>");
    html
}

fn tiles(outcome: &ReportOutcome) -> SafeHtml {
    let required: Vec<&ReportCheck> = outcome.checks.iter().filter(|c| c.required).collect();
    let optional: Vec<&ReportCheck> = outcome.checks.iter().filter(|c| !c.required).collect();
    let required_passed = required
        .iter()
        .filter(|check| check.status == CheckStatus::Passed)
        .count();
    let optional_failed = optional
        .iter()
        .filter(|check| check_look(check.status).0 == Tone::Crit)
        .count();
    let optional_unrun = optional
        .iter()
        .filter(|check| check_look(check.status).0 == Tone::Neutral)
        .count();
    let added: u32 = outcome.files.iter().map(|file| file.lines_added).sum();
    let removed: u32 = outcome.files.iter().map(|file| file.lines_removed).sum();

    let blocker_tone = if outcome.blockers.is_empty() {
        Tone::Good
    } else {
        Tone::Crit
    };
    let required_tone = if required_passed == required.len() {
        Tone::Good
    } else {
        Tone::Crit
    };
    let optional_tone = if optional_failed > 0 {
        Tone::Crit
    } else if optional_unrun > 0 {
        Tone::Neutral
    } else {
        Tone::Good
    };

    let mut html = SafeHtml::markup("<div class=\"tiles\">");
    html.push(&tile(
        blocker_tone,
        Icon::Alert,
        "Blockers",
        &outcome.blockers.len().to_string(),
        "Reported by the Integrator",
    ))
    .push(&tile(
        required_tone,
        if required_tone == Tone::Good {
            Icon::Check
        } else {
            Icon::Cross
        },
        "Required checks",
        &format!("{required_passed} / {}", required.len()),
        "passed",
    ))
    .push(&tile(
        optional_tone,
        match optional_tone {
            Tone::Crit => Icon::Cross,
            Tone::Good => Icon::Check,
            _ => Icon::Minus,
        },
        "Optional checks",
        &format!("{optional_failed} failed"),
        &format!("{optional_unrun} not run"),
    ))
    .push(&tile(
        Tone::Neutral,
        Icon::File,
        "Files",
        &outcome.files.len().to_string(),
        &format!("+{added} −{removed} lines"),
    ))
    .push_markup("</div>");
    html
}

fn criteria(outcome: &ReportOutcome) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<section id=\"criteria\" aria-labelledby=\"criteria-h\"><div class=\"sec-head\"><h2 id=\"criteria-h\">Acceptance criteria</h2><p>Grouped by verdict, worst first. Check results come from host receipts. Boxed text is the agent's own explanation.</p></div>",
    );
    for verdict in [
        Verdict::Contradicted,
        Verdict::Unverified,
        Verdict::Supported,
    ] {
        let members: Vec<(usize, &ReportCriterion)> = outcome
            .criteria
            .iter()
            .enumerate()
            .filter(|(_, criterion)| criterion.verdict == verdict)
            .collect();
        if members.is_empty() {
            continue;
        }
        let (tone, glyph, label) = verdict_look(verdict);
        html.push_markup("<div class=\"group\"><h3 class=\"group-title ")
            .push_markup(tone.class())
            .push_markup("\">")
            .push(&icon(glyph))
            .push_markup(label)
            .push_markup(" · ")
            .push_text(&members.len().to_string())
            .push_markup("</h3>");
        for (index, criterion) in members {
            html.push(&criterion_card(outcome, index, criterion));
        }
        html.push_markup("</div>");
    }
    html.push_markup("</section>");
    html
}

fn criterion_card(outcome: &ReportOutcome, index: usize, criterion: &ReportCriterion) -> SafeHtml {
    let (tone, glyph, label) = verdict_look(criterion.verdict);
    let mut html = SafeHtml::markup("<article class=\"crit ");
    html.push_markup(tone.class())
        .push_markup("\"")
        .push_attr("id", &format!("c-{}", index + 1))
        .push_markup("><div class=\"crit-head\"><span class=\"chip ")
        .push_markup(tone.class())
        .push_markup("\">")
        .push(&icon(glyph))
        .push_markup(label)
        .push_markup("</span><span class=\"ac\">")
        .push_text(&criterion.criterion_id)
        .push_markup("</span><span>")
        .push_text(&criterion.task_title)
        .push_markup("</span></div><h4>")
        .push_text(&criterion.description)
        .push_markup(
            "</h4><div class=\"claim\"><span class=\"who\">Integrator · agent-written</span>",
        )
        .push_text(&criterion.explanation)
        .push_markup("</div><dl class=\"facts\"><dt>Host checks</dt>");
    if criterion.check_refs.is_empty() {
        html.push_markup("<dd>")
            .push(&status_label(Tone::Neutral, Icon::Minus, "None cited"))
            .push_markup("</dd>");
    }
    for check_ref in &criterion.check_refs {
        html.push_markup("<dd>");
        match check_passed(outcome, check_ref) {
            Some(status) => {
                let (tone, glyph, label) = check_look(status);
                html.push_markup("<span class=\"status ")
                    .push_markup(tone.class())
                    .push_markup("\">")
                    .push(&icon(glyph))
                    .push_markup("<code>")
                    .push_text(check_ref)
                    .push_markup("</code> ")
                    .push_markup(label)
                    .push_markup("</span>");
            }
            None => {
                html.push_markup("<span class=\"status is-neutral\">")
                    .push(&icon(Icon::Minus))
                    .push_markup("<code>")
                    .push_text(check_ref)
                    .push_markup("</code> not reported</span>");
            }
        }
        html.push_markup("</dd>");
    }
    html.push_markup("</dl></article>");
    html
}

struct Gap {
    tone: Tone,
    glyph: Icon,
    title: SafeHtml,
    detail: String,
}

fn gaps(outcome: &ReportOutcome) -> Vec<Gap> {
    let mut found = Vec::new();
    for (index, criterion) in outcome.criteria.iter().enumerate() {
        let anchor = format!("#c-{}", index + 1);
        let mut title = SafeHtml::markup("<a");
        title
            .push_attr("href", &anchor)
            .push_markup(">")
            .push_text(&criterion.criterion_id)
            .push_markup("</a>");
        match criterion.verdict {
            Verdict::Unverified => {
                title.push_markup(" was not verified");
                found.push(Gap {
                    tone: Tone::Warn,
                    glyph: Icon::Help,
                    title,
                    detail: if criterion.check_refs.is_empty() {
                        "No host check was cited for it.".into()
                    } else {
                        "Its cited checks did not settle it.".into()
                    },
                });
            }
            Verdict::Supported if !backed_by_passing_check(outcome, criterion) => {
                title.push_markup(" is supported without a passing host check");
                found.push(Gap {
                    tone: Tone::Warn,
                    glyph: Icon::Help,
                    title,
                    detail: "The verdict rests on the agent's reading.".into(),
                });
            }
            _ => {}
        }
    }
    for check in &outcome.checks {
        let (tone, glyph, label) = check_look(check.status);
        if check.status == CheckStatus::Passed {
            continue;
        }
        let mut title = SafeHtml::markup("<a href=\"#checks\"><code>");
        title
            .push_text(&check.check_ref)
            .push_markup("</code></a> ")
            .push_text(&label.to_lowercase())
            .push_markup(if check.required {
                " (required)"
            } else {
                " (optional)"
            });
        found.push(Gap {
            tone,
            glyph,
            title,
            detail: match check.exit_code {
                Some(code) => format!("Exit code {code}. No passing receipt exists for it."),
                None => "It has no exit code, so nothing it would test was executed.".into(),
            },
        });
    }
    found
}

fn not_verified(outcome: &ReportOutcome) -> SafeHtml {
    let found = gaps(outcome);
    let mut html = SafeHtml::markup(
        "<section id=\"not-verified\" aria-labelledby=\"nv-h\"><div class=\"sec-head\"><h2 id=\"nv-h\">What nothing executed</h2><p>Listed with the same weight as what passed. A verdict or a declared test is not an executed check.</p></div>",
    );
    if found.is_empty() {
        html.push_markup("<div class=\"card notice is-good\">")
            .push(&icon(Icon::Check))
            .push_markup(
                "<p>Every criterion is backed by a passing host check, and every check passed.</p></div>",
            );
    } else {
        html.push_markup("<ul class=\"gaps\">");
        for gap in found {
            html.push_markup("<li class=\"")
                .push_markup(gap.tone.class())
                .push_markup("\">")
                .push(&icon(gap.glyph))
                .push_markup("<b>")
                .push(&gap.title)
                .push_markup("</b><p>")
                .push_text(&gap.detail)
                .push_markup("</p></li>");
        }
        html.push_markup("</ul>");
    }
    html.push_markup("</section>");
    html
}

fn checks(checks: &[ReportCheck]) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<section id=\"checks\" aria-labelledby=\"checks-h\"><div class=\"sec-head\"><h2 id=\"checks-h\">Host checks</h2><p>Status and exit code come from host receipts, never from an agent.</p></div>",
    );
    if checks.is_empty() {
        html.push_markup(
            "<div class=\"card notice\"><p>No host check was configured; nothing was executed.</p></div></section>",
        );
        return html;
    }
    html.push_markup(
        "<div class=\"table-wrap\"><table class=\"wide\"><thead><tr><th>Check</th><th>Required</th><th>Status</th><th>Command</th><th class=\"num\">Exit</th><th class=\"num\">Duration</th><th>Receipt</th></tr></thead><tbody>",
    );
    for check in checks {
        let (tone, glyph, label) = check_look(check.status);
        html.push_markup("<tr><td><code>")
            .push_text(&check.check_ref)
            .push_markup("</code></td><td>")
            .push_markup(if check.required {
                "Yes"
            } else {
                "<span class=\"dim\">No</span>"
            })
            .push_markup("</td><td>")
            .push(&status_label(tone, glyph, label))
            .push_markup("</td><td>");
        match &check.command {
            Some(command) => {
                html.push(&element("code", "", &SafeHtml::text(command)));
            }
            None => {
                html.push_markup("<span class=\"dim\">—</span>");
            }
        }
        html.push_markup("</td><td class=\"num\">")
            .push_text(
                &check
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "—".into()),
            )
            .push_markup("</td><td class=\"num\">")
            .push_text(
                &check
                    .duration_seconds
                    .map(check_duration)
                    .unwrap_or_else(|| "—".into()),
            )
            .push_markup("</td><td>");
        match &check.receipt_id {
            Some(receipt) => {
                html.push_markup("<code")
                    .push_attr("title", receipt)
                    .push_markup(">")
                    .push_text(short_id(receipt))
                    .push_markup("</code>");
            }
            None => {
                html.push_markup("<span class=\"dim\">—</span>");
            }
        }
        html.push_markup("</td></tr>");
    }
    html.push_markup("</tbody></table></div></section>");
    html
}

fn files(files: &[ReportFile]) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<section id=\"files\" aria-labelledby=\"files-h\"><div class=\"sec-head\"><h2 id=\"files-h\">Files</h2><p>Publication is owned by the host, which re-checks each baseline hash before writing.</p></div>",
    );
    if files.is_empty() {
        html.push_markup(
            "<div class=\"card notice\"><p>This pulsation changed no file.</p></div></section>",
        );
        return html;
    }
    let largest = files
        .iter()
        .map(|file| file.lines_added + file.lines_removed)
        .max()
        .unwrap_or(0)
        .max(1) as f64;
    html.push_markup(
        "<div class=\"table-wrap\"><table class=\"wide\"><thead><tr><th>Path</th><th>Change</th><th>Lines</th><th>Publication</th></tr></thead><tbody>",
    );
    for file in files {
        let (tone, glyph, publication) = publication_look(file.publication);
        html.push_markup("<tr><td><code>")
            .push_text(&file.path)
            .push_markup("</code></td><td><span class=\"tag\">")
            .push_markup(change_label(file.change))
            .push_markup("</span></td><td><span class=\"diff\"")
            .push_attr(
                "title",
                &format!("{} added, {} removed", file.lines_added, file.lines_removed),
            )
            .push_markup("><span class=\"n\">")
            .push_text(&format!("+{} −{}", file.lines_added, file.lines_removed))
            .push_markup("</span><span class=\"dbar\">");
        if file.lines_added > 0 {
            html.push_markup("<i class=\"a\"")
                .push_attr(
                    "style",
                    &format!("width:{:.1}%", share(f64::from(file.lines_added), largest)),
                )
                .push_markup("></i>");
        }
        if file.lines_removed > 0 {
            html.push_markup("<i class=\"d\"")
                .push_attr(
                    "style",
                    &format!(
                        "width:{:.1}%",
                        share(f64::from(file.lines_removed), largest)
                    ),
                )
                .push_markup("></i>");
        }
        html.push_markup("</span></span></td><td>")
            .push(&status_label(tone, glyph, publication))
            .push_markup("</td></tr>");
    }
    html.push_markup("</tbody></table></div></section>");
    html
}

fn run_context(run: &ReportRun) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<section id=\"run\" aria-labelledby=\"run-h\"><div class=\"sec-head\"><h2 id=\"run-h\">How this pulsation got here</h2><p>From durable usage accounting and stage history.</p></div><div class=\"charts\">",
    );
    html.push(&tokens_chart(run))
        .push(&wall_clock_chart(run))
        .push_markup("</div>")
        .push(&stage_table(run))
        .push_markup("</section>");
    html
}

fn tokens_chart(run: &ReportRun) -> SafeHtml {
    let values: Vec<(PulsationStage, f64)> = run
        .stages
        .iter()
        .map(|stage| {
            (
                stage.stage,
                (stage.input_tokens + stage.output_tokens) as f64,
            )
        })
        .collect();
    let total: f64 = values.iter().map(|(_, value)| value).sum();
    let mut html =
        SafeHtml::markup("<div class=\"card chart\"><h3>Tokens by stage</h3><p class=\"cap\">");
    html.push_text(&format!(
        "{} tokens across {} · input + output",
        compact(total),
        plural(values.len(), "stage", "stages")
    ))
    .push_markup("</p>");
    if total <= 0.0 {
        html.push_markup("<p class=\"dim\">No token usage was recorded.</p></div>");
        return html;
    }
    let ceiling = nice_ceiling(values.iter().map(|(_, value)| *value).fold(0.0, f64::max));
    let description = values
        .iter()
        .map(|(stage, value)| format!("{} {}", stage_label(*stage), compact(*value)))
        .collect::<Vec<_>>()
        .join(", ");
    html.push_markup("<div class=\"bars\" role=\"img\"")
        .push_attr("aria-label", &format!("Tokens by stage: {description}"))
        .push_markup(">");
    for (stage, value) in &values {
        let width = format!("{:.1}%", share(*value, ceiling));
        html.push_markup("<div class=\"row\"")
            .push_attr(
                "title",
                &format!("{} · {} tokens", stage_label(*stage), compact(*value)),
            )
            .push_markup("><span class=\"lbl\">")
            .push_markup(stage_label(*stage))
            .push_markup("</span><div class=\"plot\"><div class=\"bar\"")
            .push_attr("style", &format!("width:{width}"))
            .push_markup("></div><span class=\"tip\"")
            .push_attr("style", &format!("left:{width}"))
            .push_markup(">")
            .push_text(&compact(*value))
            .push_markup("</span></div></div>");
    }
    html.push_markup(
        "<span aria-hidden=\"true\"></span><div class=\"ticks\" aria-hidden=\"true\">",
    );
    for quarter in 0..=4 {
        html.push_markup("<span")
            .push_attr("style", &format!("left:{}%", quarter * 25))
            .push_markup(">")
            .push_text(&compact(ceiling * f64::from(quarter) / 4.0))
            .push_markup("</span>");
    }
    html.push_markup("</div></div></div>");
    html
}

fn wall_clock_chart(run: &ReportRun) -> SafeHtml {
    let host = (run.wall_clock_seconds
        - run.agent_seconds
        - run.review_wait_seconds
        - run.unblock_wait_seconds)
        .max(0.0);
    let parts: [(&'static str, &'static str, f64); 4] = [
        ("s1", "Agents working", run.agent_seconds),
        ("s3", "Waiting for review", run.review_wait_seconds),
        (
            "s2",
            "Waiting to unblock failures",
            run.unblock_wait_seconds,
        ),
        ("s4", "Host checks and queue", host),
    ];
    let whole = parts.iter().map(|(_, _, value)| value).sum::<f64>();
    let mut html = SafeHtml::markup(
        "<div class=\"card chart\"><h3>Where the wall clock went</h3><p class=\"cap\">Agents, waiting for a human, and everything else</p><div class=\"total\"><b>",
    );
    html.push_text(&duration(run.wall_clock_seconds))
        .push_markup("</b><span>start → last update</span></div><div class=\"stack\" role=\"img\"")
        .push_attr(
            "aria-label",
            &parts
                .iter()
                .map(|(_, label, value)| format!("{label} {}", duration(*value)))
                .collect::<Vec<_>>()
                .join(", "),
        )
        .push_markup(">");
    for &(class, label, value) in &parts {
        if value <= 0.0 {
            continue;
        }
        html.push_markup("<i class=\"")
            .push_markup(class)
            .push_markup("\"")
            .push_attr("style", &format!("flex-grow:{:.1}", share(value, whole)))
            .push_attr(
                "title",
                &format!(
                    "{label} · {} · {:.0}%",
                    duration(value),
                    share(value, whole)
                ),
            )
            .push_markup("></i>");
    }
    html.push_markup("</div><ul class=\"keys-list\">");
    for &(class, label, value) in &parts {
        html.push_markup("<li><span class=\"sw ")
            .push_markup(class)
            .push_markup("\" aria-hidden=\"true\"></span><span class=\"d\">")
            .push_markup(label)
            .push_markup("</span><span class=\"v\">")
            .push_text(&format!(
                "{} · {:.0}%",
                duration(value),
                share(value, whole)
            ))
            .push_markup("</span></li>");
    }
    html.push_markup("</ul></div>");
    html
}

fn stage_table(run: &ReportRun) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<div class=\"table-wrap\" style=\"margin-top:16px\"><table class=\"wide\"><thead><tr><th>Stage</th><th class=\"num\">Agent time</th><th class=\"num\">Discarded</th><th class=\"num\">Runs</th><th class=\"num\">Tokens in</th><th class=\"num\">Tokens out</th></tr></thead><tbody>",
    );
    for stage in &run.stages {
        html.push_markup("<tr><td>")
            .push_markup(stage_label(stage.stage))
            .push_markup("</td><td class=\"num\">")
            .push_text(&duration(stage.agent_seconds))
            .push_markup("</td><td class=\"num\">")
            .push_text(&duration(stage.discarded_seconds))
            .push_markup("</td><td class=\"num\">")
            .push_text(&stage.runs.to_string())
            .push_markup("</td><td class=\"num\">")
            .push_text(&compact(stage.input_tokens as f64))
            .push_markup("</td><td class=\"num\">")
            .push_text(&compact(stage.output_tokens as f64))
            .push_markup("</td></tr>");
    }
    let tokens = &run.tokens;
    html.push_markup("</tbody></table></div><p class=\"dim\" style=\"margin-top:10px;font-size:12.5px\">")
        .push_text(&format!(
            "All stages: {} in, {} out, {} read from cache. Discarded by failed or rejected runs: {} in, {} out.",
            compact(tokens.input as f64),
            compact(tokens.output as f64),
            compact(tokens.cache_read as f64),
            compact(tokens.discarded_input as f64),
            compact(tokens.discarded_output as f64),
        ))
        .push_markup("</p>");
    html
}

fn rejected_outputs(rejected: &[RejectedOutput]) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<section id=\"rejected\" aria-labelledby=\"rejected-h\"><div class=\"sec-head\"><h2 id=\"rejected-h\">Rejected model output</h2><p>Output the host refused after validation. Each one cost a full model call.</p></div><div class=\"table-wrap\"><table class=\"wide\"><thead><tr><th>Agent</th><th class=\"num\">Raw output</th><th>Validation error</th></tr></thead><tbody>",
    );
    for output in rejected {
        html.push_markup("<tr><td>")
            .push_text(&output.agent)
            .push_markup("</td><td class=\"num\">")
            .push_text(&format!("{} B", output.raw_output_bytes))
            .push_markup("</td><td class=\"wrap-text\">")
            .push_text(&output.error)
            .push_markup("</td></tr>");
    }
    html.push_markup("</tbody></table></div></section>");
    html
}

fn footer() -> SafeHtml {
    SafeHtml::markup(
        "<footer><span>Rendered by the host from validated payloads, host receipts and usage records. Same inputs, same bytes. Not a document of record.</span><span>No scripts, no network requests, no external assets. Agent-written text is escaped and labelled.</span></footer>",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = "src/reports/fixtures/pulsation-report.golden.html";

    fn sample() -> PulsationReport {
        let check = |check_ref: &str, required: bool, status: CheckStatus| ReportCheck {
            check_ref: check_ref.into(),
            required,
            status,
            command: None,
            exit_code: None,
            duration_seconds: None,
            receipt_id: None,
        };
        let criterion =
            |id: &str, description: &str, verdict: Verdict, checks: &[&str]| ReportCriterion {
                criterion_id: id.into(),
                description: description.into(),
                task_title: "Export usage summary to CSV".into(),
                verdict,
                explanation: format!("Explanation for {id}."),
                check_refs: checks.iter().map(|check| (*check).into()).collect(),
            };
        let stage = |stage, runs, agent, input, output| ReportStage {
            stage,
            runs,
            agent_seconds: agent,
            discarded_seconds: 0.0,
            input_tokens: input,
            output_tokens: output,
        };
        PulsationReport {
            pulsation: ReportPulsation {
                id: "5d0c2a6e-8b1f-4c3a-9e27-61f0b4a9d2c8".into(),
                feature_name: "Usage export to CSV".into(),
                status: PulsationStatus::Checkpoint,
                stage: PulsationStage::Integrator,
                pipeline_version: 4,
                created_at: "2026-10-15T13:05:00Z".into(),
                updated_at: "2026-10-15T14:32:00Z".into(),
            },
            outcome: Some(ReportOutcome {
                criteria: vec![
                    criterion(
                        "AC-1",
                        "One row per pulsation and model.",
                        Verdict::Supported,
                        &["core_tests"],
                    ),
                    criterion(
                        "AC-2",
                        "Totals equal query_usage.",
                        Verdict::Supported,
                        &["core_tests"],
                    ),
                    criterion("AC-3", "RFC 4180 quoting.", Verdict::Supported, &[]),
                    criterion(
                        "AC-4",
                        "Empty range writes only the header row.",
                        Verdict::Unverified,
                        &[],
                    ),
                    criterion(
                        "AC-5",
                        "A failed write leaves no partial file.",
                        Verdict::Contradicted,
                        &["core_tests"],
                    ),
                    criterion(
                        "AC-6",
                        "Export disabled while loading.",
                        Verdict::Unverified,
                        &["ui_layout_tests"],
                    ),
                ],
                blockers: vec![
                    "AC-5 is contradicted: write to a temporary file and rename.".into(),
                ],
                checks: vec![
                    ReportCheck {
                        command: Some("cargo test -p hvc-core usage".into()),
                        exit_code: Some(0),
                        duration_seconds: Some(72.4),
                        receipt_id: Some("7f3c9b14-0d62-4e85-a7c1-5b28e4f60a93".into()),
                        ..check("core_tests", true, CheckStatus::Passed)
                    },
                    ReportCheck {
                        command: Some("cargo clippy --workspace -- -D warnings".into()),
                        exit_code: Some(101),
                        duration_seconds: Some(57.3),
                        receipt_id: Some("e2d85a6f-91c3-4b07-8f4e-c6a0193b7d58".into()),
                        ..check("clippy", false, CheckStatus::Failed)
                    },
                    check("ui_layout_tests", false, CheckStatus::NotRun),
                ],
                files: vec![
                    ReportFile {
                        path: "crates/hvc-core/src/usage/export.rs".into(),
                        change: FileChange::Added,
                        lines_added: 164,
                        lines_removed: 0,
                        publication: PublicationState::Pending,
                    },
                    ReportFile {
                        path: "crates/hvc-ui/src/usage.rs".into(),
                        change: FileChange::Modified,
                        lines_added: 38,
                        lines_removed: 6,
                        publication: PublicationState::Conflict,
                    },
                ],
            }),
            run: ReportRun {
                wall_clock_seconds: 5220.0,
                agent_seconds: 1420.0,
                review_wait_seconds: 3430.0,
                unblock_wait_seconds: 0.0,
                tokens: ReportTokens {
                    input: 180_000,
                    output: 39_500,
                    cache_read: 92_000,
                    discarded_input: 0,
                    discarded_output: 0,
                },
                stages: vec![
                    stage(PulsationStage::Intake, 1, 64.0, 7_000, 1_400),
                    stage(PulsationStage::Specification, 2, 220.0, 26_000, 5_200),
                    stage(PulsationStage::Contract, 1, 175.0, 19_000, 3_700),
                    stage(PulsationStage::Code, 2, 552.0, 78_000, 18_500),
                    stage(PulsationStage::Test, 1, 271.0, 35_000, 6_800),
                    stage(PulsationStage::Integrator, 1, 138.0, 15_000, 3_900),
                ],
            },
            rejected_outputs: vec![RejectedOutput {
                agent: "Contract".into(),
                raw_output_bytes: 2048,
                error: "missing required key `groups`".into(),
            }],
        }
    }

    #[test]
    fn rendering_is_pure_and_matches_the_golden_file() {
        let report = sample();
        let first = render_pulsation_report(&report);
        assert_eq!(first, render_pulsation_report(&report));
        let round_trip: PulsationReport =
            serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        assert_eq!(first, render_pulsation_report(&round_trip));

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
        if std::env::var_os("HVC_UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, &first).unwrap();
        }
        let golden = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            golden == first,
            "{GOLDEN} differs from the renderer output; rerun with HVC_UPDATE_GOLDEN=1 after reviewing the change"
        );
    }

    #[test]
    fn hostile_text_is_escaped_everywhere() {
        let hostile = "</code></pre><script>alert(1)</script><img src=x onerror=alert(2)>\"'";
        let mut report = sample();
        report.pulsation.feature_name = hostile.into();
        report.pulsation.id = hostile.into();
        let outcome = report.outcome.as_mut().unwrap();
        outcome.blockers.push(hostile.into());
        for criterion in &mut outcome.criteria {
            criterion.criterion_id = hostile.into();
            criterion.description = hostile.into();
            criterion.task_title = hostile.into();
            criterion.explanation = hostile.into();
            criterion.check_refs.push(hostile.into());
        }
        for check in &mut outcome.checks {
            check.check_ref = hostile.into();
            check.command = Some(hostile.into());
            check.receipt_id = Some(hostile.into());
        }
        for file in &mut outcome.files {
            file.path = hostile.into();
        }
        report.rejected_outputs[0].agent = hostile.into();
        report.rejected_outputs[0].error = hostile.into();

        let out = render_pulsation_report(&report);
        let lowered = out.to_ascii_lowercase();
        assert!(!lowered.contains("<script"));
        assert!(!lowered.contains("<img"));
        assert!(!lowered.contains("</pre>"));
        assert!(out.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(out.contains("&quot;&#39;"));
    }

    #[test]
    fn telemetry_only_reports_say_what_is_missing() {
        let mut report = sample();
        report.outcome = None;
        let out = render_pulsation_report(&report);
        assert!(out.contains("built from run telemetry only"));
        assert!(!out.contains("id=\"criteria\""));
        assert!(!out.contains("href=\"#criteria\""));
        assert!(out.contains("id=\"run\""));
    }

    #[test]
    fn unexecuted_work_is_listed_with_the_reason() {
        let out = render_pulsation_report(&sample());
        // AC-3 is supported but cites no check; AC-4 and AC-6 are unverified.
        assert!(out.contains("AC-3</a> is supported without a passing host check"));
        assert!(out.contains("AC-4</a> was not verified"));
        assert!(out.contains("AC-6</a> was not verified"));
        assert!(out.contains("<code>clippy</code></a> failed (optional)"));
        assert!(out.contains("<code>ui_layout_tests</code></a> not run (optional)"));
        assert!(!out.contains("<code>core_tests</code></a> passed"));
        // Contradicted criteria lead the criteria list.
        let contradicted = out.find("Contradicted · 1").unwrap();
        let supported = out.find("Supported · 3").unwrap();
        assert!(contradicted < supported);
    }

    #[test]
    fn number_formatting_is_stable() {
        assert_eq!(compact(8_400.0), "8.4K");
        assert_eq!(compact(25_000.0), "25K");
        assert_eq!(compact(1_260_000.0), "1.3M");
        assert_eq!(compact(980.0), "980");
        assert_eq!(nice_ceiling(96_500.0), 100_000.0);
        assert_eq!(nice_ceiling(21_000.0), 25_000.0);
        assert_eq!(nice_ceiling(0.0), 1.0);
        assert_eq!(duration(5220.0), "1 h 27 min");
        assert_eq!(duration(1420.0), "23 min 40 s");
        assert_eq!(duration(41.4), "41 s");
        assert_eq!(check_duration(2.14), "2.1 s");
    }
}
