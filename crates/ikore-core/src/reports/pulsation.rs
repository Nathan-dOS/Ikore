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
    /// What the composed Integrator reviewed (ADR-032); `None` before
    /// pipeline v4 or when nothing was recorded.
    #[serde(default)]
    pub review: Option<ReportReview>,
    /// Human revision rounds next to technical attempts (ADR-032 M4).
    #[serde(default)]
    pub convergence: Option<ReportConvergence>,
    /// Risk level and coverage of the latest review (ADR-034 M3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<ReportRisk>,
    /// The trust layers when the report was rendered (ADR-035 M2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<ReportLayers>,
    /// The attestation and how it checked when the report was rendered
    /// (ADR-036 M3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ReportProvenance>,
}

/// The pulsation's attestation. It is the record; the report only repeats
/// it and how it checks now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportProvenance {
    pub state: String,
    pub key_id: Option<String>,
    pub statement_sha256: Option<String>,
    pub sealed_at: Option<String>,
    pub signature: Option<String>,
    pub rederivation: Option<String>,
    /// Why sealing failed, when it did.
    pub diagnostic: Option<String>,
    pub subjects: Vec<ReportSubject>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportSubject {
    pub path: String,
    pub sha256: String,
    /// The file now: unchanged, changed since the attestation, missing or
    /// unreadable.
    pub workspace: String,
}

/// The host's trust layers, as the pulsation detail shows them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportLayers {
    pub policy_version: u32,
    pub layers: Vec<ReportLayer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportLayer {
    pub name: String,
    pub state: String,
    pub headline: String,
    pub reasons: Vec<String>,
    pub items: Vec<ReportLayerItem>,
    /// Items left out after the per-layer limit.
    pub more: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportLayerItem {
    pub label: String,
    pub value: String,
}

/// The risk the host assessed and what backed each criterion and check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportRisk {
    pub level: String,
    pub reasons: Vec<String>,
    pub unknown_inputs: Vec<String>,
    pub rows: Vec<ReportCoverageRow>,
    pub missing_kinds: Vec<String>,
    pub acknowledgements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportCoverageRow {
    pub kind: String,
    pub subject: String,
    pub state: String,
    pub detail: String,
}

/// The host's record of the reviewer's inputs, without hashes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportReview {
    pub tool_scope: String,
    pub policy_state: String,
    pub policy_files: Vec<String>,
    pub candidates: usize,
    pub changed_outside_pipeline: Vec<String>,
    pub author_claims: usize,
    pub gaps: Vec<ReportGap>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportGap {
    pub code: String,
    pub subject: String,
    pub limits_verdicts: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportConvergence {
    pub rounds: u32,
    pub threshold: u32,
    pub human_rounds: u32,
    pub failure_revisions: u32,
    pub retries: u32,
    pub technical_attempts: u32,
    /// `decision: reason`, in order.
    pub decisions: Vec<String>,
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
    /// `None` when the host cannot count them: it keeps no copy of a modified
    /// file's previous version, so the report says so instead of guessing.
    pub lines: Option<LineCounts>,
    pub publication: PublicationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineCounts {
    pub added: u32,
    pub removed: u32,
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
    /// Plan-window change measured around this pulsation's CLI runs (ADR-028).
    #[serde(default)]
    pub plan_quota: Vec<ReportPlanQuota>,
}

/// The used-percent change of one CLI plan window over the pulsation's runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportPlanQuota {
    pub cli: String,
    pub window: String,
    pub delta_percent: f64,
    /// Another run of the same CLI overlapped one of these runs.
    pub approximate: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportTokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub discarded_input: i64,
    pub discarded_output: i64,
    /// The paired, provider-reported cache breakdown; absent in older reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_measurement: Option<ReportCacheMeasurement>,
    /// Arithmetic saturation means the displayed token totals are not exact.
    #[serde(default, skip_serializing_if = "is_false")]
    pub totals_overflow: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Cache coverage is separate from the all-input token totals. Counts and
/// normalized sums remain integral so the report can validate the ratio before
/// rendering it; the ratio is never accepted from a serialized floating value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReportCacheMeasurement {
    pub state: String,
    pub basis: String,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub records: u64,
    pub eligible_records: u64,
    pub partial_records: u64,
    pub unknown_records: u64,
    pub invalid_records: u64,
    pub unavailable_records: u64,
    pub zero_input_records: u64,
    pub overflow: bool,
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
    fn omitted() -> String {
        document(
            "Report unavailable",
            "Report content was omitted because sanitization could not be completed.",
            &SafeHtml::text(crate::secret_redactor::OMITTED_CONTENT),
        )
    }
    let Ok(original_json) = super::analysis::canonical_report_json(report) else {
        return omitted();
    };
    // Decode each field before host formatting combines it with labels. A
    // JSON-escaped credential must not become opaque inside a title/tooltip.
    let observed = observed_report(report);
    let Ok(observed_json) = super::analysis::canonical_report_json(&observed) else {
        return omitted();
    };
    let report = &observed;
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
    if report.review.is_some() || report.convergence.is_some() {
        page.push(&review_section(
            report.review.as_ref(),
            report.convergence.as_ref(),
        ));
    }
    if let Some(risk) = &report.risk {
        page.push(&risk_section(risk));
    }
    if let Some(layers) = &report.layers {
        page.push(&layers_section(layers));
    }
    if let Some(provenance) = &report.provenance {
        page.push(&provenance_section(provenance));
    }
    page.push(&run_context(&report.run));
    if !report.rejected_outputs.is_empty() {
        page.push(&rejected_outputs(&report.rejected_outputs));
    }
    if original_json != observed_json {
        page.push_markup("<div hidden id=\"sanitization-metadata\"")
            .push_attr("data-policy", crate::secret_redactor::SANITIZATION_POLICY_VERSION)
            .push_attr("data-original-evidence-sha256", &crate::secret_redactor::text_sha256(&original_json))
            .push_attr("data-sanitized-content-sha256", &crate::secret_redactor::text_sha256(&observed_json))
            .push_attr("data-hash-scope", "UTF-8 compact JSON of the typed PulsationReport with recursively sorted object keys and original array order; hashes exclude this metadata and the final HTML bytes.")
            .push_markup("></div>");
    }
    page.push_markup("</main>")
        .push(&footer())
        .push_markup("</div>");
    let output = document(
        &format!("Pulsation report · {}", pulsation.feature_name),
        &format!(
            "Host-rendered report of pulsation {} ({}).",
            pulsation.id, pulsation.feature_name
        ),
        &page,
    );
    if output.len() > crate::secret_redactor::MAX_SANITIZATION_BYTES {
        omitted()
    } else {
        output
    }
}

fn observed_report(report: &PulsationReport) -> PulsationReport {
    fn text(value: &mut String) {
        *value = crate::sanitization::sanitized_observation(value);
    }
    fn optional(value: &mut Option<String>) {
        if let Some(value) = value {
            text(value);
        }
    }
    fn strings(values: &mut [String]) {
        for value in values {
            text(value);
        }
    }
    let mut report = report.clone();
    text(&mut report.pulsation.id);
    text(&mut report.pulsation.feature_name);
    text(&mut report.pulsation.created_at);
    text(&mut report.pulsation.updated_at);
    if let Some(outcome) = &mut report.outcome {
        strings(&mut outcome.blockers);
        for criterion in &mut outcome.criteria {
            text(&mut criterion.criterion_id);
            text(&mut criterion.description);
            text(&mut criterion.task_title);
            text(&mut criterion.explanation);
            strings(&mut criterion.check_refs);
        }
        for check in &mut outcome.checks {
            text(&mut check.check_ref);
            optional(&mut check.command);
            optional(&mut check.receipt_id);
        }
        for file in &mut outcome.files {
            text(&mut file.path);
        }
    }
    if let Some(cache) = &mut report.run.tokens.cache_measurement {
        text(&mut cache.state);
        text(&mut cache.basis);
    }
    for quota in &mut report.run.plan_quota {
        text(&mut quota.cli);
        text(&mut quota.window);
    }
    for output in &mut report.rejected_outputs {
        text(&mut output.agent);
        text(&mut output.error);
    }
    if let Some(review) = &mut report.review {
        strings(&mut review.policy_files);
        strings(&mut review.changed_outside_pipeline);
        for gap in &mut review.gaps {
            text(&mut gap.subject);
        }
    }
    if let Some(convergence) = &mut report.convergence {
        strings(&mut convergence.decisions);
    }
    if let Some(layers) = &mut report.layers {
        for layer in &mut layers.layers {
            text(&mut layer.headline);
            strings(&mut layer.reasons);
            for item in &mut layer.items {
                text(&mut item.label);
                text(&mut item.value);
            }
        }
    }
    if let Some(risk) = &mut report.risk {
        strings(&mut risk.reasons);
        strings(&mut risk.unknown_inputs);
        strings(&mut risk.missing_kinds);
        strings(&mut risk.acknowledgements);
        for row in &mut risk.rows {
            text(&mut row.subject);
            text(&mut row.detail);
        }
    }
    report
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
        "<header><div class=\"top\"><div class=\"brand\"><div class=\"mark\" aria-hidden=\"true\">IK</div><div><b>Ikore</b><span>Pulsation report</span></div></div><div class=\"ids\"><span>Pulsation <code",
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
    if report.review.is_some() || report.convergence.is_some() {
        html.push_markup("<li><a href=\"#review\">Review inputs</a></li>");
    }
    if report.risk.is_some() {
        html.push_markup("<li><a href=\"#risk\">Risk and coverage</a></li>");
    }
    if report.layers.is_some() {
        html.push_markup("<li><a href=\"#layers\">Layers</a></li>");
    }
    if report.provenance.is_some() {
        html.push_markup("<li><a href=\"#provenance\">Provenance</a></li>");
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
    let counted: Vec<LineCounts> = outcome.files.iter().filter_map(|file| file.lines).collect();
    let added: u32 = counted.iter().map(|lines| lines.added).sum();
    let removed: u32 = counted.iter().map(|lines| lines.removed).sum();
    let lines = if counted.len() == outcome.files.len() {
        format!("+{added} −{removed} lines")
    } else if counted.is_empty() {
        "Line counts unavailable".to_owned()
    } else {
        format!(
            "+{added} −{removed} lines in {} of {}",
            counted.len(),
            outcome.files.len()
        )
    };

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
        &lines,
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
        // A check name stays whole; a long command breaks instead.
        html.push_markup("<tr><td class=\"nowrap\"><code>")
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
        .filter_map(|file| file.lines)
        .map(|lines| lines.added + lines.removed)
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
            .push_markup("</span></td><td>");
        match file.lines {
            Some(lines) => {
                html.push_markup("<span class=\"diff\"")
                    .push_attr(
                        "title",
                        &format!("{} added, {} removed", lines.added, lines.removed),
                    )
                    .push_markup("><span class=\"n\">")
                    .push_text(&format!("+{} −{}", lines.added, lines.removed))
                    .push_markup("</span><span class=\"dbar\">");
                if lines.added > 0 {
                    html.push_markup("<i class=\"a\"")
                        .push_attr(
                            "style",
                            &format!("width:{:.1}%", share(f64::from(lines.added), largest)),
                        )
                        .push_markup("></i>");
                }
                if lines.removed > 0 {
                    html.push_markup("<i class=\"d\"")
                        .push_attr(
                            "style",
                            &format!("width:{:.1}%", share(f64::from(lines.removed), largest)),
                        )
                        .push_markup("></i>");
                }
                html.push_markup("</span></span>");
            }
            None => {
                html.push_markup(
                    "<span class=\"dim\" title=\"The host keeps no copy of the previous version, so it cannot count the changed lines.\">Not counted</span>",
                );
            }
        }
        html.push_markup("</td><td>")
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
                stage.input_tokens as f64 + stage.output_tokens as f64,
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
    let cache_description = if tokens.cache_measurement.is_some() {
        "cache-read tokens reported"
    } else {
        "read from cache"
    };
    html.push_markup(
        "</tbody></table></div><p class=\"dim\" style=\"margin-top:10px;font-size:12.5px\">",
    )
    .push_text(&format!(
        "All stages: {} in, {} out, {} {}. Discarded by failed or rejected runs: {} in, {} out.",
        compact(tokens.input as f64),
        compact(tokens.output as f64),
        compact(tokens.cache_read as f64),
        cache_description,
        compact(tokens.discarded_input as f64),
        compact(tokens.discarded_output as f64),
    ))
    .push_markup("</p>");
    if tokens.totals_overflow {
        html.push_markup("<p class=\"dim\" id=\"token-overflow\">")
            .push_text("Token totals saturated after an overflow and are not exact.")
            .push_markup("</p>");
    }
    if let Some(cache) = &tokens.cache_measurement {
        let ratio = match (cache.input_tokens, cache.cache_read_tokens) {
            (Some(input), Some(read))
                if !cache.overflow
                    && cache.basis == "reported_normalized_input"
                    && cache.eligible_records > 0
                    && matches!(cache.state.as_str(), "known" | "partial")
                    && input > 0
                    && read >= 0
                    && read <= input =>
            {
                super::analysis::ratio(read as f64, input as f64)
                    .map(|percentage| format!("{percentage:.1}%"))
                    .unwrap_or_else(|| "unavailable".to_owned())
            }
            (Some(0), Some(0))
                if !cache.overflow
                    && cache.basis == "reported_normalized_input"
                    && cache.state == "not_applicable" =>
            {
                "not applicable".to_owned()
            }
            _ => "unavailable".to_owned(),
        };
        html.push_markup("<p class=\"dim\" id=\"cache-measurement\">")
            .push_text(&format!(
                "Cache reuse: {ratio} of eligible reported input. Coverage: {}/{} eligible; {} unknown; {} invalid; {} unavailable; {} partial; {} zero-input. State: {}. Basis: {}.",
                cache.eligible_records,
                cache.records,
                cache.unknown_records,
                cache.invalid_records,
                cache.unavailable_records,
                cache.partial_records,
                cache.zero_input_records,
                if cache.state.is_empty() { "unknown" } else { &cache.state },
                if cache.basis.is_empty() { "unavailable" } else { &cache.basis },
            ))
            .push_markup("</p>");
    }
    if !run.plan_quota.is_empty() {
        let parts = run
            .plan_quota
            .iter()
            .map(|quota| {
                format!(
                    "{} {} {:+.1}%{}",
                    quota.cli,
                    quota.window,
                    quota.delta_percent,
                    if quota.approximate { " (approx.)" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        html.push_markup(
            "<p class=\"dim\" id=\"plan-quota\" style=\"margin-top:4px;font-size:12.5px\">",
        )
        .push_text(&format!(
            "Plan usage during this pulsation's CLI runs: {parts}. Usage elsewhere on the same account in the same minutes also counts."
        ))
        .push_markup("</p>");
    }
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

/// ADR-032: what the reviewer received and how many revision rounds it took.
fn review_section(
    review: Option<&ReportReview>,
    convergence: Option<&ReportConvergence>,
) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<section id=\"review\" aria-labelledby=\"review-h\"><div class=\"sec-head\"><h2 id=\"review-h\">Review inputs</h2><p>What the host gave the final reviewer, what it could not establish, and the revision rounds. A gap that limits verdicts turned any supported verdict resting on it into unverified.</p></div><div class=\"card\"><dl class=\"facts\">",
    );
    let fact = |html: &mut SafeHtml, label: &str, value: &str| {
        html.push_markup("<dt>")
            .push_text(label)
            .push_markup("</dt><dd>")
            .push_text(value)
            .push_markup("</dd>");
    };
    if let Some(review) = review {
        fact(
            &mut html,
            "Reviewer context",
            if review.tool_scope == "host_context_only" {
                "Host-selected context only"
            } else {
                "Workspace snapshot"
            },
        );
        let policy = if review.policy_files.is_empty() {
            review.policy_state.clone()
        } else {
            format!(
                "{} ({})",
                review.policy_state,
                review.policy_files.join(", ")
            )
        };
        fact(&mut html, "REVIEW.md", &policy);
        fact(&mut html, "Candidate files", &review.candidates.to_string());
        if !review.changed_outside_pipeline.is_empty() {
            fact(
                &mut html,
                "Changed after publication",
                &review.changed_outside_pipeline.join(", "),
            );
        }
        fact(
            &mut html,
            "Author claims",
            &review.author_claims.to_string(),
        );
    }
    if let Some(convergence) = convergence {
        fact(
            &mut html,
            "Revision rounds",
            &format!(
                "{} since the last replan (threshold {}) · {} at checkpoints, {} on failures, {} retries · {} technical attempts",
                convergence.rounds,
                convergence.threshold,
                convergence.human_rounds,
                convergence.failure_revisions,
                convergence.retries,
                convergence.technical_attempts
            ),
        );
        for decision in &convergence.decisions {
            fact(&mut html, "Decision", decision);
        }
    }
    html.push_markup("</dl></div>");
    if let Some(review) = review.filter(|review| !review.gaps.is_empty()) {
        html.push_markup(
            "<div class=\"table-wrap\"><table class=\"wide\"><thead><tr><th>Evidence gap</th><th>Effect</th><th>Detail</th></tr></thead><tbody>",
        );
        for gap in &review.gaps {
            html.push_markup("<tr><td>")
                .push_text(&gap.code)
                .push_markup("</td><td>")
                .push_text(if gap.limits_verdicts {
                    "Limits verdicts"
                } else {
                    "Shown only"
                })
                .push_markup("</td><td class=\"wrap-text\">")
                .push_text(&gap.subject)
                .push_markup("</td></tr>");
        }
        html.push_markup("</tbody></table></div>");
    }
    html.push_markup("</section>");
    html
}

/// ADR-034 M3: the risk level, its reasons and what backed each criterion
/// and each check the level required.
fn risk_section(risk: &ReportRisk) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<section id=\"risk\" aria-labelledby=\"risk-h\"><div class=\"sec-head\"><h2 id=\"risk-h\">Risk and coverage</h2><p>The level the host assessed adds scrutiny and never removes a stage or checkpoint. Executed rows rest on a fresh host receipt; model-only rows rest on the reviewer's reading alone.</p></div><div class=\"card\"><dl class=\"facts\">",
    );
    let fact = |html: &mut SafeHtml, label: &str, value: &str| {
        html.push_markup("<dt>")
            .push_text(label)
            .push_markup("</dt><dd>")
            .push_text(value)
            .push_markup("</dd>");
    };
    fact(&mut html, "Risk level", &risk.level);
    for reason in &risk.reasons {
        fact(&mut html, "Reason", reason);
    }
    if !risk.unknown_inputs.is_empty() {
        fact(&mut html, "Unknown inputs", &risk.unknown_inputs.join(", "));
    }
    if !risk.missing_kinds.is_empty() {
        fact(
            &mut html,
            "Required but not configured",
            &risk.missing_kinds.join(", "),
        );
    }
    for acknowledgement in &risk.acknowledgements {
        fact(&mut html, "Acknowledged", acknowledgement);
    }
    html.push_markup("</dl></div>");
    if !risk.rows.is_empty() {
        html.push_markup(
            "<div class=\"table-wrap\"><table class=\"wide\"><thead><tr><th>Item</th><th>Coverage</th><th>Detail</th></tr></thead><tbody>",
        );
        for row in &risk.rows {
            html.push_markup("<tr><td>")
                .push_text(&format!("{} {}", row.kind, row.subject))
                .push_markup("</td><td>")
                .push_text(&row.state)
                .push_markup("</td><td class=\"wrap-text\">")
                .push_text(&row.detail)
                .push_markup("</td></tr>");
        }
        html.push_markup("</tbody></table></div>");
    }
    html.push_markup("</section>");
    html
}

fn layers_section(layers: &ReportLayers) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<section id=\"layers\" aria-labelledby=\"layers-h\"><div class=\"sec-head\"><h2 id=\"layers-h\">Layers</h2><p>Derived by the host from its own records when this report was rendered. Clear means no problem the host can detect, not that the change is safe; the layers decide nothing.</p></div><div class=\"table-wrap\"><table class=\"wide\"><thead><tr><th>Layer</th><th>State</th><th>Summary</th></tr></thead><tbody>",
    );
    for layer in &layers.layers {
        html.push_markup("<tr><td>")
            .push_text(&layer.name)
            .push_markup("</td><td>")
            .push_text(&layer.state)
            .push_markup("</td><td class=\"wrap-text\">")
            .push_text(&layer.headline)
            .push_markup("</td></tr>");
    }
    html.push_markup("</tbody></table></div>");
    for layer in &layers.layers {
        if layer.reasons.is_empty() && layer.items.is_empty() {
            continue;
        }
        html.push_markup("<div class=\"card\"><dl class=\"facts\"><dt>Layer</dt><dd>")
            .push_text(&layer.name)
            .push_markup("</dd>");
        for reason in &layer.reasons {
            html.push_markup("<dt>Why</dt><dd>")
                .push_text(reason)
                .push_markup("</dd>");
        }
        for item in &layer.items {
            html.push_markup("<dt>")
                .push_text(&item.label)
                .push_markup("</dt><dd>")
                .push_text(&item.value)
                .push_markup("</dd>");
        }
        if layer.more > 0 {
            html.push_markup("<dt>More</dt><dd>")
                .push_text(&format!("{} more", layer.more))
                .push_markup("</dd>");
        }
        html.push_markup("</dl></div>");
    }
    html.push_markup("</section>");
    html
}

fn provenance_section(provenance: &ReportProvenance) -> SafeHtml {
    let mut html = SafeHtml::markup(
        "<section id=\"provenance\" aria-labelledby=\"provenance-h\"><div class=\"sec-head\"><h2 id=\"provenance-h\">Provenance</h2><p>The signed attestation is the record of what this pulsation applied; this report repeats it and how it checked when rendered. A file changed since the attestation differs now; that says nothing about who changed it.</p></div><div class=\"card\"><dl class=\"facts\"><dt>State</dt><dd>",
    );
    html.push_text(&provenance.state).push_markup("</dd>");
    let facts = [
        ("Key", provenance.key_id.as_deref()),
        ("Statement", provenance.statement_sha256.as_deref()),
        ("Sealed", provenance.sealed_at.as_deref()),
        ("Signature", provenance.signature.as_deref()),
        ("Re-derivation", provenance.rederivation.as_deref()),
        ("Sealing error", provenance.diagnostic.as_deref()),
    ];
    for (label, value) in facts {
        if let Some(value) = value {
            html.push_markup("<dt>")
                .push_text(label)
                .push_markup("</dt><dd class=\"wrap-text\">")
                .push_text(&if label == "Statement" {
                    format!("sha256:{value}")
                } else {
                    value.to_owned()
                })
                .push_markup("</dd>");
        }
    }
    html.push_markup("</dl></div>");
    if !provenance.subjects.is_empty() {
        html.push_markup(
            "<div class=\"table-wrap\"><table class=\"wide\"><thead><tr><th>Subject</th><th>SHA-256</th><th>Workspace now</th></tr></thead><tbody>",
        );
        for subject in &provenance.subjects {
            html.push_markup("<tr><td class=\"wrap-text\">")
                .push_text(&subject.path)
                .push_markup("</td><td class=\"wrap-text\">")
                .push_text(&subject.sha256)
                .push_markup("</td><td>")
                .push_text(&subject.workspace)
                .push_markup("</td></tr>");
        }
        html.push_markup("</tbody></table></div>");
    }
    html.push_markup("</section>");
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

    fn frontier_cache_metadata() -> serde_json::Value {
        serde_json::json!({
            "state": "partial",
            "basis": "reported_normalized_input",
            "input_tokens": 100,
            "cache_read_tokens": 50,
            "records": 2,
            "eligible_records": 1,
            "partial_records": 0,
            "unknown_records": 1,
            "invalid_records": 0,
            "unavailable_records": 0,
            "zero_input_records": 0,
            "overflow": false
        })
    }

    fn frontier_report_with_cache(metadata: serde_json::Value) -> PulsationReport {
        // Exercise the same typed JSON boundary that report generation uses.
        let mut value = serde_json::to_value(sample()).unwrap();
        value["run"]["tokens"]["cache_measurement"] = metadata;
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn frontier_html_cache_legacy_reports_keep_absent_measurement_unknown() {
        let mut value = serde_json::to_value(sample()).unwrap();
        let tokens = value["run"]["tokens"].as_object_mut().unwrap();
        tokens.remove("cache_measurement");
        tokens.remove("totals_overflow");
        let report: PulsationReport = serde_json::from_value(value).unwrap();
        assert_eq!(report, sample());
        let html = render_pulsation_report(&report);
        assert!(!html.contains("Cache reuse:"));
        assert!(!html.contains("Token totals saturated"));
    }

    #[test]
    fn frontier_html_cache_reports_only_the_eligible_paired_input_with_coverage() {
        let report = frontier_report_with_cache(frontier_cache_metadata());
        let html = render_pulsation_report(&report);
        if let Some(path) = std::env::var_os("HVC_FRONTIER_REPORT_PREVIEW") {
            std::fs::write(path, &html).unwrap();
        }
        assert!(html.contains("Cache reuse: 50.0% of eligible reported input."));
        assert!(html.contains(
            "Coverage: 1/2 eligible; 1 unknown; 0 invalid; 0 unavailable; 0 partial; 0 zero-input."
        ));
        assert!(html.contains("Basis: reported_normalized_input."));
        assert!(html.contains("cache-read tokens reported"));
        // The 180K all-input total in sample() must not dilute the 100-token
        // denominator whose cache breakdown was actually observed.
        assert!(!html.contains("Cache reuse: 0.0%"));
    }

    #[test]
    fn frontier_html_cache_does_not_invent_a_ratio_for_unknown_zero_or_invalid_data() {
        let variants = [
            ("unknown", Some(100), None, false, "unavailable"),
            ("not_applicable", Some(0), Some(0), false, "not applicable"),
            ("known", Some(100), Some(101), false, "unavailable"),
            ("known", Some(-1), Some(0), false, "unavailable"),
            ("known", Some(100), Some(-1), false, "unavailable"),
            ("known", Some(100), Some(50), true, "unavailable"),
            ("unknown", Some(100), Some(50), false, "unavailable"),
        ];
        for (state, input, read, overflow, expected) in variants {
            let mut metadata = frontier_cache_metadata();
            metadata["state"] = serde_json::json!(state);
            metadata["input_tokens"] = serde_json::json!(input);
            metadata["cache_read_tokens"] = serde_json::json!(read);
            metadata["overflow"] = serde_json::json!(overflow);
            let html = render_pulsation_report(&frontier_report_with_cache(metadata));
            assert!(
                html.contains(&format!(
                    "Cache reuse: {expected} of eligible reported input."
                )),
                "{state}: {html}"
            );
            assert!(!html.contains("Cache reuse: 0.0%"), "{state}");
            assert!(!html.contains("Cache reuse: 50.0%"), "{state}");
        }
        // Plausible values with an unrecognized accounting basis cannot
        // establish either a numeric ratio or a zero-input conclusion.
        for (state, input, read) in [("partial", 100, 50), ("not_applicable", 0, 0)] {
            let mut metadata = frontier_cache_metadata();
            metadata["state"] = serde_json::json!(state);
            metadata["basis"] = serde_json::json!("unsupported_accounting_basis");
            metadata["input_tokens"] = serde_json::json!(input);
            metadata["cache_read_tokens"] = serde_json::json!(read);
            let html = render_pulsation_report(&frontier_report_with_cache(metadata));
            assert!(html.contains("Cache reuse: unavailable of eligible reported input."));
            assert!(!html.contains("Cache reuse: 50.0%"));
            assert!(!html.contains("Cache reuse: not applicable"));
        }
        // A known, explicitly reported zero remains a valid observation.
        let mut zero = frontier_cache_metadata();
        zero["state"] = serde_json::json!("known");
        zero["cache_read_tokens"] = serde_json::json!(0);
        assert!(
            render_pulsation_report(&frontier_report_with_cache(zero))
                .contains("Cache reuse: 0.0% of eligible reported input.")
        );
    }

    #[test]
    fn frontier_html_cache_escapes_untrusted_state_and_basis() {
        let hostile = "</p><script>alert(1)</script><img src=x onerror=alert(2)>";
        let mut metadata = frontier_cache_metadata();
        metadata["state"] = serde_json::json!(hostile);
        metadata["basis"] = serde_json::json!(hostile);
        let html = render_pulsation_report(&frontier_report_with_cache(metadata));
        assert!(!html.contains("<script"));
        assert!(!html.contains("<img"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("Cache reuse: unavailable"));
        assert!(html.contains("default-src &#39;none&#39;"));
    }

    #[test]
    fn frontier_html_token_overflow_warns_without_losing_non_token_evidence() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["run"]["tokens"]["totals_overflow"] = serde_json::json!(true);
        let report: PulsationReport = serde_json::from_value(value).unwrap();
        let html = render_pulsation_report(&report);
        assert!(html.contains("Token totals saturated after an overflow and are not exact."));
        assert!(html.contains("One row per pulsation and model."));
        assert!(html.contains("id=\"criteria\""));
    }

    #[test]
    fn frontier_html_maximum_stage_tokens_do_not_overflow_the_chart() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["run"]["tokens"]["input"] = serde_json::json!(i64::MAX);
        value["run"]["tokens"]["output"] = serde_json::json!(i64::MAX);
        value["run"]["tokens"]["totals_overflow"] = serde_json::json!(true);
        value["run"]["stages"][0]["input_tokens"] = serde_json::json!(i64::MAX);
        value["run"]["stages"][0]["output_tokens"] = serde_json::json!(i64::MAX);
        let report: PulsationReport = serde_json::from_value(value).unwrap();
        let html = render_pulsation_report(&report);
        assert!(html.contains("Token totals saturated after an overflow and are not exact."));
        assert!(html.contains("Tokens by stage"));
        assert!(html.contains("One row per pulsation and model."));
        assert!(!html.contains("NaN"));
        assert!(!html.contains("inf%"));
    }

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
                        command: Some("cargo test -p ikore-core usage".into()),
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
                        path: "crates/ikore-core/src/usage/export.rs".into(),
                        change: FileChange::Added,
                        lines: Some(LineCounts {
                            added: 164,
                            removed: 0,
                        }),
                        publication: PublicationState::Pending,
                    },
                    ReportFile {
                        path: "crates/ikore-ui/src/usage.rs".into(),
                        change: FileChange::Modified,
                        lines: Some(LineCounts {
                            added: 38,
                            removed: 6,
                        }),
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
                    cache_measurement: None,
                    totals_overflow: false,
                },
                stages: vec![
                    stage(PulsationStage::Intake, 1, 64.0, 7_000, 1_400),
                    stage(PulsationStage::Specification, 2, 220.0, 26_000, 5_200),
                    stage(PulsationStage::Contract, 1, 175.0, 19_000, 3_700),
                    stage(PulsationStage::Code, 2, 552.0, 78_000, 18_500),
                    stage(PulsationStage::Test, 1, 271.0, 35_000, 6_800),
                    stage(PulsationStage::Integrator, 1, 138.0, 15_000, 3_900),
                ],
                plan_quota: Vec::new(),
            },
            rejected_outputs: vec![RejectedOutput {
                agent: "Contract".into(),
                raw_output_bytes: 2048,
                error: "missing required key `groups`".into(),
            }],
            review: None,
            convergence: None,
            risk: None,
            layers: Some(ReportLayers {
                policy_version: 1,
                layers: vec![
                    layer(
                        "Identity",
                        "attention",
                        "8 run(s) on 2 model(s) · mandate revision 1",
                        &["`code_implementer` ran `gpt-4.1`, not the requested `gpt-4.1-mini`."],
                        &[(
                            "code_implementer",
                            "api · requested gpt-4.1-mini · observed gpt-4.1 · 2 run(s)",
                        )],
                        0,
                    ),
                    layer(
                        "Scope",
                        "issue",
                        "4 granted · 1 written · 0 refused",
                        &[
                            "1 publication(s) in conflict: the workspace kept the developer's version.",
                        ],
                        &[("crates/ikore-ui/src/usage.rs", "conflict")],
                        0,
                    ),
                    layer(
                        "Guardrails",
                        "clear",
                        "1/1 required check(s) passed",
                        &[],
                        &[],
                        0,
                    ),
                    layer(
                        "Intent",
                        "issue",
                        "6 criteria · 6 with a validated unit · risk medium · 2 supported",
                        &["The Integrator found 1 criterion(s) contradicted."],
                        &[("AC-5", "contradicted")],
                        3,
                    ),
                ],
            }),
            provenance: Some(ReportProvenance {
                state: "signed".into(),
                key_id: Some(format!("hvc-ed25519:{}", "0f1e2d3c".repeat(8))),
                statement_sha256: Some("ab".repeat(32)),
                sealed_at: Some("2026-10-26T12:45:05Z".into()),
                signature: Some("valid".into()),
                rederivation: Some("matches".into()),
                diagnostic: None,
                subjects: vec![
                    ReportSubject {
                        path: "crates/ikore-core/src/usage/export.rs".into(),
                        sha256: "cd".repeat(32),
                        workspace: "unchanged".into(),
                    },
                    ReportSubject {
                        path: "crates/ikore-ui/src/usage.rs".into(),
                        sha256: "ef".repeat(32),
                        workspace: "changed since the attestation".into(),
                    },
                ],
            }),
        }
    }

    /// ADR-036 M3: the provenance section repeats the attestation and how
    /// it checks; reports written before it still parse.
    #[test]
    fn the_provenance_section_repeats_the_attestation_and_older_reports_still_parse() {
        let report = sample();
        let html = render_pulsation_report(&report);
        assert!(html.contains("<a href=\"#provenance\">Provenance</a>"));
        assert!(html.contains("id=\"provenance\""));
        assert!(html.contains(&format!("sha256:{}", "ab".repeat(32))));
        assert!(html.contains("changed since the attestation"));
        assert!(html.contains("says nothing about who changed it"));
        let mut earlier = serde_json::to_value(&report).unwrap();
        earlier.as_object_mut().unwrap().remove("provenance");
        assert_eq!(
            serde_json::from_value::<PulsationReport>(earlier).unwrap(),
            PulsationReport {
                provenance: None,
                ..report
            }
        );
        let mut without = sample();
        without.provenance = None;
        assert!(!render_pulsation_report(&without).contains("id=\"provenance\""));
    }

    fn layer(
        name: &str,
        state: &str,
        headline: &str,
        reasons: &[&str],
        items: &[(&str, &str)],
        more: usize,
    ) -> ReportLayer {
        ReportLayer {
            name: name.into(),
            state: state.into(),
            headline: headline.into(),
            reasons: reasons.iter().map(|reason| (*reason).into()).collect(),
            items: items
                .iter()
                .map(|(label, value)| ReportLayerItem {
                    label: (*label).into(),
                    value: (*value).into(),
                })
                .collect(),
            more,
        }
    }

    /// ADR-035 M2: the layers section lists every layer with its state and
    /// says what clear means; reports written before it still parse.
    #[test]
    fn the_layers_section_lists_each_layer_and_older_reports_still_parse() {
        let report = sample();
        let html = render_pulsation_report(&report);
        assert!(html.contains("<a href=\"#layers\">Layers</a>"));
        assert!(html.contains("id=\"layers\""));
        assert!(html.contains("not that the change is safe"));
        for name in ["Identity", "Scope", "Guardrails", "Intent"] {
            assert!(html.contains(&format!("<tr><td>{name}</td>")), "{name}");
        }
        assert!(html.contains("<dt>More</dt><dd>3 more</dd>"));
        let mut earlier = serde_json::to_value(&report).unwrap();
        earlier.as_object_mut().unwrap().remove("layers");
        assert_eq!(
            serde_json::from_value::<PulsationReport>(earlier).unwrap(),
            PulsationReport {
                layers: None,
                ..report
            }
        );
        let mut without = sample();
        without.layers = None;
        assert!(!render_pulsation_report(&without).contains("id=\"layers\""));
    }

    /// ADR-032: the review section appears only when recorded, says what the
    /// reviewer received and sanitizes what it quotes.
    #[test]
    fn the_review_section_reports_inputs_gaps_and_rounds() {
        let mut report = sample();
        let plain = render_pulsation_report(&report);
        assert!(!plain.contains("id=\"review\""));
        report.review = Some(ReportReview {
            tool_scope: "host_context_only".into(),
            policy_state: "loaded".into(),
            policy_files: vec!["REVIEW.md".into()],
            candidates: 2,
            changed_outside_pipeline: vec!["src/lib.rs".into()],
            author_claims: 3,
            gaps: vec![ReportGap {
                code: "candidate_changed_outside_pipeline".into(),
                subject:
                    "`src/lib.rs` changed after publication; token sk-proj-abcdefghijklmnopqrst"
                        .into(),
                limits_verdicts: true,
            }],
        });
        report.convergence = Some(ReportConvergence {
            rounds: 3,
            threshold: 3,
            human_rounds: 2,
            failure_revisions: 1,
            retries: 0,
            technical_attempts: 9,
            decisions: vec!["continue_beyond_threshold: The boundary case is still wrong.".into()],
        });
        let html = render_pulsation_report(&report);
        assert!(html.contains("<a href=\"#review\">Review inputs</a>"));
        assert!(html.contains("id=\"review\""));
        assert!(html.contains("Host-selected context only"));
        assert!(html.contains("loaded (REVIEW.md)"));
        assert!(html.contains("candidate_changed_outside_pipeline"));
        assert!(html.contains("Limits verdicts"));
        assert!(html.contains("3 since the last replan (threshold 3)"));
        assert!(html.contains("The boundary case is still wrong."));
        assert!(!html.contains("sk-proj-abcdefghijklmnopqrst"));
        // Reports written before ADR-032 still parse.
        let mut earlier = serde_json::to_value(sample()).unwrap();
        earlier.as_object_mut().unwrap().remove("review");
        earlier.as_object_mut().unwrap().remove("convergence");
        assert_eq!(
            serde_json::from_value::<PulsationReport>(earlier).unwrap(),
            sample()
        );
    }

    /// ADR-034 M3: the risk section appears only when recorded, lists each
    /// coverage state and sanitizes what it quotes.
    #[test]
    fn the_risk_section_reports_the_level_and_the_coverage() {
        let mut report = sample();
        let plain = render_pulsation_report(&report);
        assert!(!plain.contains("id=\"risk\""));
        report.risk = Some(ReportRisk {
            level: "high".into(),
            reasons: vec!["sensitive path `src/auth/session.rs` (high)".into()],
            unknown_inputs: vec!["intake:task_2".into()],
            rows: vec![
                ReportCoverageRow {
                    kind: "criterion".into(),
                    subject: "crit_1_1".into(),
                    state: "executed".into(),
                    detail: "unit: passed".into(),
                },
                ReportCoverageRow {
                    kind: "criterion".into(),
                    subject: "crit_1_2".into(),
                    state: "model_only".into(),
                    detail: "supported by the model's review only".into(),
                },
                ReportCoverageRow {
                    kind: "check".into(),
                    subject: "lint".into(),
                    state: "not_run".into(),
                    detail: "token sk-proj-abcdefghijklmnopqrst".into(),
                },
            ],
            missing_kinds: vec!["lint".into()],
            acknowledgements: vec!["2 pending: The lint profile is set up next sprint.".into()],
        });
        let html = render_pulsation_report(&report);
        assert!(html.contains("<a href=\"#risk\">Risk and coverage</a>"));
        assert!(html.contains("id=\"risk\""));
        assert!(html.contains("<dd>high</dd>"));
        assert!(html.contains("sensitive path `src/auth/session.rs` (high)"));
        assert!(html.contains("intake:task_2"));
        assert!(html.contains("model_only"));
        assert!(html.contains("Required but not configured"));
        assert!(html.contains("The lint profile is set up next sprint."));
        assert!(!html.contains("sk-proj-abcdefghijklmnopqrst"));
        // Reports written before ADR-034 still parse, as having no risk.
        let mut earlier = serde_json::to_value(sample()).unwrap();
        assert!(earlier.as_object().unwrap().get("risk").is_none());
        earlier.as_object_mut().unwrap().remove("risk");
        assert_eq!(
            serde_json::from_value::<PulsationReport>(earlier).unwrap(),
            sample()
        );
    }

    #[test]
    fn rendering_is_pure_and_matches_the_golden_file() {
        let report = sample();
        let first = render_pulsation_report(&report);
        assert!(!first.contains("id=\"sanitization-metadata\""));
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
    fn reports_sanitize_fields_before_formatting_without_changing_the_typed_record() {
        let mut report = sample();
        let encoded = r#"{"message":"sk-proj-\u0061bcdefghijklmnopqrst"}"#;
        report.pulsation.feature_name = encoded.into();
        report.rejected_outputs[0].error = encoded.into();
        let outcome = report.outcome.as_mut().unwrap();
        outcome.criteria[0].description = "Authorization: Bearer synthetic-credential-value".into();
        outcome.criteria[0].task_title = encoded.into();
        outcome.criteria[0].explanation = "ghp_syntheticabcdefghijklmnop".into();
        let original = serde_json::to_string(&report).unwrap();
        let original_sha = crate::secret_redactor::text_sha256(&original);
        let id = report.pulsation.id.clone();
        let receipt = report.outcome.as_ref().unwrap().checks[0]
            .receipt_id
            .clone();
        let output = render_pulsation_report(&report);
        let original_envelope_sha = crate::secret_redactor::text_sha256(
            &super::super::analysis::canonical_report_json(&report).unwrap(),
        );
        let observed_envelope_sha = crate::secret_redactor::text_sha256(
            &super::super::analysis::canonical_report_json(&observed_report(&report)).unwrap(),
        );
        assert_ne!(original_envelope_sha, observed_envelope_sha);
        assert!(output.contains("<div hidden id=\"sanitization-metadata\""));
        assert!(output.contains(&format!(
            "data-original-evidence-sha256=\"{original_envelope_sha}\""
        )));
        assert!(output.contains(&format!(
            "data-sanitized-content-sha256=\"{observed_envelope_sha}\""
        )));
        assert!(output.contains("hashes exclude this metadata and the final HTML bytes"));
        for forbidden in [
            "bcdefghijklmnopqrst",
            "synthetic-credential-value",
            "ghp_synthetic",
            "\\u0061",
        ] {
            assert!(
                !output.contains(forbidden),
                "report exposed a credential representation"
            );
        }
        assert!(output.contains("&lt;REDACTED_SECRET:"));
        assert!(output.contains("Content-Security-Policy"));
        assert!(!output.contains("<script"));
        assert_eq!(report.pulsation.id, id);
        assert_eq!(
            report.outcome.as_ref().unwrap().checks[0].receipt_id,
            receipt
        );
        assert_eq!(
            crate::secret_redactor::text_sha256(&serde_json::to_string(&report).unwrap()),
            original_sha
        );
    }

    #[test]
    fn report_serialization_overflow_returns_only_a_neutral_omission() {
        let mut report = sample();
        report.pulsation.feature_name = format!(
            "private-sentinel {}",
            "x".repeat(crate::secret_redactor::MAX_SANITIZATION_BYTES)
        );
        let output = render_pulsation_report(&report);
        assert!(output.contains(crate::secret_redactor::OMITTED_CONTENT));
        assert!(!output.contains("private-sentinel"));
        assert!(!output.contains(&report.pulsation.id));
        assert!(!output.contains("sanitization-metadata"));
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
        report.run.plan_quota.push(ReportPlanQuota {
            cli: hostile.into(),
            window: hostile.into(),
            delta_percent: 4.0,
            approximate: true,
        });
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
    fn a_file_without_line_counts_says_why_instead_of_showing_zero() {
        let mut report = sample();
        let outcome = report.outcome.as_mut().unwrap();
        outcome.files[1].lines = None;
        let out = render_pulsation_report(&report);
        assert!(out.contains("+164 −0 lines in 1 of 2"));
        assert!(out.contains(">Not counted</span>"));
        assert!(out.contains("cannot count the changed lines"));
        assert!(!out.contains("+0 −0"));

        for file in &mut report.outcome.as_mut().unwrap().files {
            file.lines = None;
        }
        let out = render_pulsation_report(&report);
        assert!(out.contains("Line counts unavailable"));
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
