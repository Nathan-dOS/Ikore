use gpui::{AppContext as _, Entity, TestAppContext};
use ikore_test_support::CoreTestHarness;

use crate::AppModel;

pub(crate) struct GpuiTestHarness {
    core: CoreTestHarness,
}

impl GpuiTestHarness {
    pub(crate) fn new() -> Self {
        Self {
            core: CoreTestHarness::new(),
        }
    }

    pub(crate) fn mount(&self, cx: &mut TestAppContext) -> Entity<AppModel> {
        let core = self.core.core();
        cx.new(|cx| AppModel::new(core, cx))
    }

    pub(crate) fn data_dir(&self) -> &std::path::Path {
        self.core.data_dir()
    }
}

/// A blocking finding of `revision` against one delivered decision.
pub(crate) fn blocking_finding(
    feature: &str,
    revision: u64,
) -> ikore_core::feature_forge::preflight::PreflightFinding {
    use ikore_core::feature_forge::preflight::*;
    PreflightFinding {
        id: "finding".into(),
        feature_id: feature.into(),
        feature_revision: revision,
        inputs: DecisionInputs {
            state: DecisionInputsState::Loaded,
            files: vec![DecisionFile {
                handle: Some("decision_1".into()),
                path: "knowledge/obsidian/decisions/a-native.md".into(),
                state: DecisionFileState::Loaded,
                sha256: Some("digest".into()),
                bytes: 64,
                content: None,
            }],
            omitted: vec![],
        },
        inputs_sha256: "digest".into(),
        conflicts: vec![ArchitectureConflict {
            decision_ref: "decision_1".into(),
            conflict: "The feature adds an Electron helper process.".into(),
            severity: ConflictSeverity::Blocking,
        }],
        blocking: 1,
        resolution: PreflightResolution::Pending,
        reason: None,
        proposal_feature_id: None,
        created_at: "2026-10-25T12:00:00Z".into(),
    }
}

/// One note whose linked file changed, with or without a proposal to review.
pub(crate) fn drift_overview(
    with_proposal: bool,
) -> ikore_core::doc_reconciliation::DocDriftOverview {
    use ikore_core::doc_reconciliation::*;
    let note = "knowledge/obsidian/features/api.md";
    DocDriftOverview {
        notes: vec![DriftedNote {
            note_path: note.into(),
            signals: vec![DriftSignal {
                id: "signal".into(),
                note_path: note.into(),
                code_path: "src/api.rs".into(),
                base_sha256: Some("base".into()),
                current_sha256: Some("current".into()),
                origin: DriftOrigin::Watcher,
                pulsation_id: None,
                state: if with_proposal {
                    SignalState::Proposed
                } else {
                    SignalState::Open
                },
                updated_at: "2026-10-25T12:00:00Z".into(),
            }],
            proposal: with_proposal.then(|| ReconciliationProposal {
                id: "proposal".into(),
                note_path: note.into(),
                state: ProposalState::Pending,
                expected_note_sha256: "note".into(),
                expected_code: vec![ExpectedCode {
                    path: "src/api.rs".into(),
                    sha256: Some("current".into()),
                }],
                change_summary: vec!["The API now returns MessagePack.".into()],
                edits: vec![],
                unresolved: vec!["Whether clients still send JSON.".into()],
                code_changes: vec![CodeChangeV1 {
                    path: "src/api.rs".into(),
                    change: ChangeKind::Modified,
                    diff: Some("@@ -1 +1 @@\n-json\n+msgpack\n".into()),
                    current: None,
                    gap: None,
                }],
                proposed_content: "# API\nThe API returns MessagePack.\n".into(),
                preview_diff: "@@ -1,2 +1,2 @@\n # API\n-The API returns JSON.\n+The API returns MessagePack.\n".into(),
                reason: None,
                updated_at: "2026-10-25T12:00:00Z".into(),
            }),
        }],
        tracked_links: 3,
        unlinked_notes: 2,
        silenced: vec![SilencedLink {
            note_path: note.into(),
            code_path: "src/legacy.rs".into(),
        }],
        scan: None,
        generating: vec![],
        failures: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn mounts_app_model_with_an_isolated_core(cx: &mut TestAppContext) {
        let harness = GpuiTestHarness::new();
        let model = harness.mount(cx);
        cx.run_until_parked();

        assert!(
            harness
                .data_dir()
                .join(ikore_core::app_names::DATABASE_FILE_NAME)
                .is_file()
        );
        cx.read(|cx| {
            assert_eq!(model.read(cx).active_view(), crate::AppView::Home);
        });
    }
}
