//! Host-rendered, self-contained HTML reports (ADR-027).
//!
//! A report is a derived view of data the host already validated. It is never
//! a document of record, carries no script, loads nothing from the network and
//! renders deterministically: the same input always yields the same bytes.

pub mod html;
pub mod pulsation;

pub use html::{CONTENT_SECURITY_POLICY, SafeHtml, escape};
pub use pulsation::{
    FileChange, PulsationReport, RejectedOutput, ReportCheck, ReportCriterion, ReportFile,
    ReportOutcome, ReportPulsation, ReportRun, ReportStage, ReportTokens, render_pulsation_report,
};
