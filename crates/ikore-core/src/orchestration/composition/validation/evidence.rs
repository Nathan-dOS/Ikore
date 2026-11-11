//! Host-only evidence for qualified Cargo runs. The v1 command/receipt wire
//! contracts remain unchanged; these details are persisted beside a receipt.

use serde::{Deserialize, Serialize};

use super::super::{LogExcerptV1, ReceiptStatus, Sha256Hex, Timestamp, Uuid};
use crate::orchestration::SchemaV1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationPhase {
    Preparation,
    Execution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterpretationStatus {
    Supported,
    Partial,
    Uninterpreted,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    Passed,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparationEvidence {
    pub status: ReceiptStatus,
    pub exit_code: Option<i32>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub stdout: LogExcerptV1,
    pub stderr: LogExcerptV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolVersion {
    pub tool: String,
    pub version: Option<String>,
}

/// Unknown results are intentional: successful process exit is insufficient
/// when preparation, interpretation or candidate integrity is unresolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunEvidenceDetailsV1 {
    pub schema_version: SchemaV1,
    pub receipt_id: Uuid,
    pub policy_sha256: Sha256Hex,
    pub parser_version: u32,
    pub phase: ValidationPhase,
    pub interpretation: InterpretationStatus,
    pub result: CheckResult,
    pub candidate_integrity: bool,
    pub preparation: Option<PreparationEvidence>,
    pub tool_versions: Vec<ToolVersion>,
    pub tests_passed: Option<u64>,
    pub tests_failed: Option<u64>,
    pub tests_ignored: Option<u64>,
    pub limitations: Vec<String>,
}

impl RunEvidenceDetailsV1 {
    pub fn qualifies(&self) -> bool {
        self.executed() && self.result == CheckResult::Passed
    }

    /// Completed, understood execution can be negative evidence. It does not
    /// approve a failed check or make inconclusive execution cover a criterion.
    pub fn executed(&self) -> bool {
        self.phase == ValidationPhase::Execution
            && self.interpretation == InterpretationStatus::Supported
            && matches!(self.result, CheckResult::Passed | CheckResult::Failed)
            && self.candidate_integrity
            && self
                .preparation
                .as_ref()
                .is_some_and(|preparation| preparation.status == ReceiptStatus::Passed)
            && self.tool_versions.len() == 3
            && self.tool_versions.iter().all(|tool| tool.version.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualification_requires_executed_interpreted_immutable_evidence() {
        let mut details = RunEvidenceDetailsV1 {
            schema_version: SchemaV1,
            receipt_id: Uuid::generate(),
            policy_sha256: Sha256Hex::of(b"policy"),
            parser_version: 1,
            phase: ValidationPhase::Execution,
            interpretation: InterpretationStatus::Supported,
            result: CheckResult::Passed,
            candidate_integrity: true,
            preparation: Some(PreparationEvidence {
                status: ReceiptStatus::Passed,
                exit_code: Some(0),
                started_at: Some(Timestamp::now()),
                finished_at: Some(Timestamp::now()),
                stdout: LogExcerptV1 {
                    text: String::new(),
                    truncated: false,
                },
                stderr: LogExcerptV1 {
                    text: String::new(),
                    truncated: false,
                },
            }),
            tool_versions: ["cargo", "rustc", "rustdoc"]
                .into_iter()
                .map(|tool| ToolVersion {
                    tool: tool.into(),
                    version: Some(format!("{tool} fixture")),
                })
                .collect(),
            tests_passed: Some(0),
            tests_failed: Some(0),
            tests_ignored: Some(0),
            limitations: Vec::new(),
        };
        assert!(details.qualifies());
        details.result = CheckResult::Failed;
        assert!(details.executed());
        assert!(!details.qualifies());
        details.result = CheckResult::Unknown;
        assert!(!details.executed());
        details.result = CheckResult::Passed;
        details.interpretation = InterpretationStatus::Partial;
        assert!(!details.qualifies());
        details.interpretation = InterpretationStatus::Supported;
        details.candidate_integrity = false;
        assert!(!details.qualifies());
        details.candidate_integrity = true;
        details.preparation = None;
        assert!(!details.qualifies());
    }
}
