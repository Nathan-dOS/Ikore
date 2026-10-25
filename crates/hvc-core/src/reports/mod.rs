//! Host-rendered, self-contained HTML reports (ADR-027).
//!
//! A report is a derived view of data the host already validated. It is never
//! a document of record, carries no script, loads nothing from the network and
//! renders deterministically: the same input always yields the same bytes.

pub mod analysis;
pub mod html;
pub mod pulsation;
pub mod store;

pub use html::{CONTENT_SECURITY_POLICY, SafeHtml, escape};
pub use pulsation::{
    FileChange, LineCounts, PulsationReport, RejectedOutput, ReportCheck, ReportConvergence,
    ReportCriterion, ReportFile, ReportGap, ReportOutcome, ReportPlanQuota, ReportPulsation,
    ReportReview, ReportRun, ReportStage, ReportTokens, render_pulsation_report,
};
