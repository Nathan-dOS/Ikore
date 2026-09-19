//! Host validation of a composed stage (ADR-022, M4; contract:
//! `Stage-Composition-Validation-Contract.md`).
//!
//! The host - never a model - decides which developer-configured commands
//! run, materializes a disposable candidate from the fenced baseline plus the
//! validated unit outputs, launches the command with a reduced environment
//! under a process-tree job, drains bounded logs, parses diagnostics and
//! writes a receipt. This is a trusted-project runner: it limits accidental
//! interference (disposable directory, minimal environment, timeouts, tree
//! termination) and is not a security sandbox.

pub mod candidate;
pub mod diagnostics;
pub mod profile;
pub mod runner;

pub use candidate::{Candidate, materialize_candidate};
pub use profile::{ValidationProfileSnapshot, detect_presets, resolve_profile};
pub use runner::{ReceiptContext, run_check};
