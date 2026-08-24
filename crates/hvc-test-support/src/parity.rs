use std::collections::{HashMap, HashSet};

use serde::Deserialize;

const LEDGER_JSON: &str = include_str!("../../../parity/gpui-frontend-ledger.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParityArea {
    ShellLifecycle,
    RustContracts,
    WorkspaceEditor,
    Pulsations,
    Vault,
    Settings,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParityStatus {
    Covered,
    Partial,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParityCase {
    pub id: String,
    pub area: ParityArea,
    pub source: String,
    pub target: String,
    pub status: ParityStatus,
    pub rationale: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParityLedger {
    pub baseline_frontend_tests: usize,
    pub cases: Vec<ParityCase>,
}

impl ParityLedger {
    pub fn embedded() -> Self {
        serde_json::from_str(LEDGER_JSON).expect("embedded GPUI parity ledger should be valid")
    }

    pub fn area_counts(&self) -> HashMap<ParityArea, usize> {
        let mut counts = HashMap::new();
        for case in &self.cases {
            *counts.entry(case.area).or_insert(0) += 1;
        }
        counts
    }

    pub fn covered_count(&self) -> usize {
        self.cases
            .iter()
            .filter(|case| case.status == ParityStatus::Covered)
            .count()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.baseline_frontend_tests != 51 || self.cases.len() != 51 {
            return Err(format!(
                "ledger must contain the 51-test baseline, declared={} actual={}",
                self.baseline_frontend_tests,
                self.cases.len()
            ));
        }
        let mut ids = HashSet::new();
        let mut sources = HashSet::new();
        for case in &self.cases {
            if !ids.insert(case.id.as_str()) {
                return Err(format!("duplicate parity id: {}", case.id));
            }
            if !sources.insert(case.source.as_str()) {
                return Err(format!("duplicate frontend test: {}", case.source));
            }
            if case.target.trim().is_empty() || case.rationale.trim().is_empty() {
                return Err(format!("case {} has an incomplete mapping", case.id));
            }
        }
        let expected = [
            (ParityArea::ShellLifecycle, 8),
            (ParityArea::RustContracts, 12),
            (ParityArea::WorkspaceEditor, 10),
            (ParityArea::Pulsations, 2),
            (ParityArea::Vault, 4),
            (ParityArea::Settings, 6),
            (ParityArea::Chat, 9),
        ];
        let counts = self.area_counts();
        for (area, count) in expected {
            if counts.get(&area).copied().unwrap_or_default() != count {
                return Err(format!("unexpected count for {area:?}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_ledger_maps_the_complete_51_test_baseline() {
        let ledger = ParityLedger::embedded();
        ledger.validate().unwrap();
        assert!(ledger.covered_count() < 51);
    }

    #[test]
    fn stage_one_covers_every_shell_lifecycle_case() {
        let ledger = ParityLedger::embedded();
        let shell_cases = ledger
            .cases
            .iter()
            .filter(|case| case.area == ParityArea::ShellLifecycle)
            .collect::<Vec<_>>();

        assert_eq!(shell_cases.len(), 8);
        assert!(
            shell_cases
                .iter()
                .all(|case| case.status == ParityStatus::Covered)
        );
    }

    #[test]
    fn stage_two_covers_every_workspace_editor_case() {
        let ledger = ParityLedger::embedded();
        let workspace_cases = ledger
            .cases
            .iter()
            .filter(|case| case.area == ParityArea::WorkspaceEditor)
            .collect::<Vec<_>>();

        assert_eq!(workspace_cases.len(), 10);
        assert!(
            workspace_cases
                .iter()
                .all(|case| case.status == ParityStatus::Covered)
        );
    }
}
