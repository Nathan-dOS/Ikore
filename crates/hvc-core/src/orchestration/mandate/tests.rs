use super::*;
use crate::feature_forge::{
    AcceptanceCriterion, BatchManifest, Feature, FeatureBrief, FeatureEvaluation, FeatureState,
    TaskDraft, TaskKind, WorkTask,
};
use crate::orchestration::composition::recipes::{StagePayloadRecord, UnitPayloadRecord};
use crate::orchestration::composition::{
    CheckKind, CompletedPayloadV1, ExecutorKind, Handle, ProposalKind, ReferenceEntryV1, RelPath,
    SourceRole, SpecificationContractV1, Stage, UnitModelResponseV1, UnitProposalV1,
};
use crate::orchestration::{ContentSha256, RelativeSourcePath, SchemaV1, SourceRevision};

fn h(value: &str) -> Handle {
    Handle::new(value).unwrap()
}

fn sha() -> String {
    "0".repeat(64)
}

fn manifest() -> BatchManifest {
    let task = |id: &str, title: &str, description: &str| WorkTask {
        id: id.into(),
        project_id: "project".into(),
        revision: 2,
        draft: TaskDraft {
            title: title.into(),
            kind: TaskKind::Feature,
            description: description.into(),
            criteria: vec![AcceptanceCriterion {
                id: "AC-1".into(),
                description: "It works.".into(),
            }],
            dependencies: vec![],
            feature_id: None,
        },
        feature_revision: None,
        state: "executing".into(),
        active_pulsation_id: None,
        completed_pulsation_id: None,
    };
    BatchManifest {
        schema_version: 1,
        title: "Login".into(),
        path_confirmation_source: crate::feature_forge::PathConfirmationSource::default(),
        tasks: vec![
            task(
                "t-2",
                "Document the flow",
                "Update docs/auth.md and REVIEW.md.",
            ),
            task(
                "t-1",
                "Reject expired tokens",
                "Change src/auth.rs so expired tokens fail.",
            ),
        ],
        features: vec![Feature {
            id: "f-1".into(),
            project_id: "project".into(),
            title: "Auth".into(),
            original_idea: "idea".into(),
            revision: 1,
            approved_revision: Some(1),
            state: FeatureState::Executing,
            brief: FeatureBrief {
                objective: "Secure login.".into(),
                sources: vec![SourceRevision {
                    project_id: "project".into(),
                    relative_path: RelativeSourcePath::new("SRC/Session.rs").unwrap(),
                    content_sha256: ContentSha256::of(b"session"),
                }],
                ..Default::default()
            },
            evaluation: FeatureEvaluation::default(),
            structural_level: 0,
            wave: None,
            position: 0,
            archive_reason: None,
        }],
        feature_dependencies: vec![],
    }
}

fn specification() -> StagePayloadRecord {
    let digest = || serde_json::from_value(serde_json::json!(sha())).unwrap();
    StagePayloadRecord {
        schema_version: SchemaV1,
        stage: Stage::Specification,
        plan_sha256: digest(),
        references: vec![
            ReferenceEntryV1::ExistingSource {
                handle: h("src_1"),
                relative_path: RelPath::new("src/auth.rs").unwrap(),
                content_sha256: digest(),
                source_role: SourceRole::Source,
            },
            ReferenceEntryV1::PlannedDestination {
                handle: h("plan_1"),
                relative_path: RelPath::new("src/expiry.rs").unwrap(),
                source_role: SourceRole::Source,
            },
            ReferenceEntryV1::PlannedDestination {
                handle: h("plan_2"),
                relative_path: RelPath::new("src/clock.ts").unwrap(),
                source_role: SourceRole::Source,
            },
        ],
        units: vec![UnitPayloadRecord {
            logical_unit_key: h("specify_batch"),
            response: UnitModelResponseV1::Completed {
                executor_kind: ExecutorKind::SpecificationAuthor,
                payload: CompletedPayloadV1::Specification(SpecificationContractV1 {
                    objective: "o".into(),
                    exclusions: vec![],
                    decisions: vec![],
                    requirements: vec![],
                    existing_file_refs: vec![h("src_1")],
                    planned_file_refs: vec![h("plan_1")],
                    interface_intent: vec![],
                    unit_proposals: vec![UnitProposalV1 {
                        unit_key: h("app_code"),
                        kind: ProposalKind::Code,
                        task_refs: vec![h("task_1")],
                        criterion_refs: vec![h("crit_1_1")],
                        input_refs: vec![],
                        destination_refs: vec![h("plan_2")],
                        depends_on: vec![],
                        objective: "Implement.".into(),
                    }],
                }),
            },
        }],
    }
}

pub(super) fn issued_by(checkpoint: &str) -> IssuedBy {
    IssuedBy {
        checkpoint_id: checkpoint.into(),
        stage: "SPECIFICATION".into(),
        state_version: 7,
        input_revision: 1,
    }
}

fn transports() -> Vec<TransportCapability> {
    vec![
        TransportCapability {
            role: "test_agent".into(),
            transport: "cli".into(),
            backend: Some("claude".into()),
        },
        TransportCapability {
            role: "backend_agent".into(),
            transport: "api".into(),
            backend: None,
        },
    ]
}

fn checks() -> Vec<CheckCapability> {
    vec![
        CheckCapability {
            check_ref: "check_2".into(),
            kind: CheckKind::Test,
            profile_sha256: "b".repeat(64),
            required: true,
        },
        CheckCapability {
            check_ref: "check_1".into(),
            kind: CheckKind::Compile,
            profile_sha256: "a".repeat(64),
            required: true,
        },
    ]
}

fn budget() -> BudgetParameters {
    BudgetParameters {
        stage_output_limits: vec![StageOutputLimit {
            stage: "Code".into(),
            output_tokens: 16_000,
        }],
        context_chars_limit: 16_000,
        extra_invocation_limit: 2,
        per_unit_extra_invocation_limit: 1,
        max_in_flight_units: 1,
    }
}

fn issue_with(
    enforcement: Enforcement,
    previous: Option<(&ExecutionMandateV1, &str)>,
) -> ExecutionMandateV1 {
    let manifest = manifest();
    let specification = specification();
    let clarifications = vec!["docs/answer.md".to_owned()];
    let accepted = vec!["c".repeat(64), "d".repeat(64)];
    derive_issued(&IssueInputs {
        pulsation_id: "pulse",
        project_id: "project",
        enforcement,
        title: "Login flow",
        issued_by: issued_by("cp-spec"),
        previous,
        manifest: Some(&manifest),
        clarification_paths: &clarifications,
        specification: Some(&specification),
        accepted_artifact_sha256s: &accepted,
        review_policy_snapshot_sha256: Some("e".repeat(64)),
        project_sensitive_sha256: None,
        transports: transports(),
        checks: checks(),
        budget: budget(),
    })
    .unwrap()
}

pub(super) fn issue() -> ExecutionMandateV1 {
    issue_with(Enforcement::EnforcedV4, None)
}

pub(super) fn grant<'a>(mandate: &'a ExecutionMandateV1, path: &str) -> &'a WriteGrant {
    mandate
        .scope
        .grants
        .iter()
        .find(|grant| grant.path == path)
        .unwrap_or_else(|| panic!("`{path}` is not granted: {:?}", mandate.scope.grants))
}

#[test]
fn the_same_inputs_give_the_same_bytes_and_digest() {
    let first = issue();
    let second = issue();
    assert_eq!(
        first.canonical_bytes().unwrap(),
        second.canonical_bytes().unwrap()
    );
    assert_eq!(first.sha256().unwrap(), second.sha256().unwrap());
    let bytes = first.canonical_bytes().unwrap();
    assert!(
        !bytes.contains(": ") && !bytes.contains(", "),
        "no whitespace"
    );
    assert!(
        bytes.starts_with("{\"budget\":"),
        "keys are sorted: {bytes}"
    );
}

#[test]
fn the_envelope_holds_every_path_a_v4_stage_can_own_and_only_those() {
    let mandate = issue();
    use WriteClass::*;
    assert_eq!(grant(&mandate, "SRC/Session.rs").origins, vec![BriefSource]);
    assert_eq!(
        grant(&mandate, "src/auth.rs").origins,
        vec![TaskPath, SpecificationReference]
    );
    assert_eq!(grant(&mandate, "docs/auth.md").origins, vec![TaskPath]);
    assert_eq!(
        grant(&mandate, "docs/answer.md").origins,
        vec![ClarificationPath]
    );
    assert_eq!(
        grant(&mandate, "src/expiry.rs").origins,
        vec![SpecificationReference]
    );
    assert_eq!(
        grant(&mandate, "src/clock.ts").origins,
        vec![SpecificationReference],
        "a proposal's destination is granted"
    );
    assert_eq!(
        grant(&mandate, "knowledge/obsidian/features/login-flow.md").origins,
        vec![StageDocument]
    );
    assert_eq!(
        grant(&mandate, "knowledge/obsidian/tests/login-flow.md").origins,
        vec![StageDocument]
    );
    for destination in ["tests/login_flow_test.rs", "tests/login_flow_test.ts"] {
        assert_eq!(grant(&mandate, destination).origins, vec![TestDestination]);
    }
    assert!(
        !mandate.grants_path("REVIEW.md"),
        "a review policy is never a destination"
    );
    assert!(!mandate.grants_path("src/other.rs"));
    let keys = mandate
        .scope
        .grants
        .iter()
        .map(|grant| path_key(&grant.path))
        .collect::<Vec<_>>();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(keys, sorted, "grants are unique and sorted by identity");
}

#[test]
fn paths_match_by_windows_identity() {
    let mandate = issue();
    assert!(mandate.grants_path("src/session.rs"));
    assert!(mandate.grants_path("SRC\\SESSION.RS"));
    assert!(mandate.grants_path("Src/Auth.rs"));
}

/// The identity is the v4 owner table's (`RelPath::canonical_key`): ordinal,
/// ASCII-only case folding. A non-ASCII case variant is another key and is
/// refused, never granted by accident; a reserved device name is never a
/// grant, because grants are built from `RelPath`.
#[test]
fn unicode_and_reserved_names_fail_closed() {
    let mut mandate = issue();
    mandate.scope.grants.push(WriteGrant {
        path: "docs/ação.md".into(),
        origins: vec![WriteClass::TaskPath],
        unit_key: None,
        amendment_revision: None,
    });
    let verdict = |path| check_write(Some(&mandate), WriteSource::PipelineStage, path);
    assert_eq!(verdict("DOCS\\ação.md"), WriteVerdict::Allowed);
    assert!(matches!(verdict("docs/AÇÃO.md"), WriteVerdict::Refused(_)));
    assert_eq!(
        path_key("Docs/Ação.md"),
        RelPath::new("Docs/Ação.md").unwrap().canonical_key()
    );
    for reserved in ["src/nul.rs", "CON", "docs/Com1.md"] {
        assert!(RelPath::new(reserved).is_err(), "`{reserved}`");
        assert!(
            matches!(verdict(reserved), WriteVerdict::Refused(_)),
            "`{reserved}`"
        );
    }
}

#[test]
fn intent_capabilities_and_budget_are_recorded_sorted() {
    let mandate = issue();
    assert_eq!(mandate.schema_version, MANDATE_SCHEMA_VERSION);
    assert_eq!(mandate.revision, 1);
    assert_eq!(mandate.previous_sha256, None);
    assert_eq!(mandate.cause, MandateCause::Issued);
    assert_eq!(mandate.expires_at, None);
    let tasks = mandate
        .intent
        .task_revisions
        .iter()
        .map(|task| (task.task_id.as_str(), task.revision))
        .collect::<Vec<_>>();
    assert_eq!(tasks, vec![("t-1", 2), ("t-2", 2)]);
    assert_eq!(
        mandate.intent.specification_sha256,
        digest_of_set(&["d".repeat(64), "c".repeat(64)]).unwrap()
    );
    assert_eq!(
        mandate
            .capabilities
            .transports
            .iter()
            .map(|t| t.role.as_str())
            .collect::<Vec<_>>(),
        vec!["backend_agent", "test_agent"]
    );
    assert_eq!(
        mandate
            .capabilities
            .checks
            .iter()
            .map(|c| c.check_ref.as_str())
            .collect::<Vec<_>>(),
        vec!["check_1", "check_2"]
    );
    assert_eq!(
        mandate.capabilities.executors.len(),
        ExecutorKind::ALL.len()
    );
    assert_eq!(mandate.budget.output_limit_for("CODE"), Some(16_000));
    assert_eq!(mandate.budget.output_limit_for("Test"), None);
}

#[test]
fn a_task_draft_change_changes_its_digest_and_the_mandate() {
    let base = issue();
    let mut manifest = manifest();
    manifest.tasks[0]
        .draft
        .description
        .push_str(" Also docs/extra.md.");
    let specification = specification();
    let accepted = vec!["c".repeat(64)];
    let changed = derive_issued(&IssueInputs {
        pulsation_id: "pulse",
        project_id: "project",
        enforcement: Enforcement::EnforcedV4,
        title: "Login flow",
        issued_by: issued_by("cp-spec"),
        previous: None,
        manifest: Some(&manifest),
        clarification_paths: &[],
        specification: Some(&specification),
        accepted_artifact_sha256s: &accepted,
        review_policy_snapshot_sha256: None,
        project_sensitive_sha256: None,
        transports: transports(),
        checks: checks(),
        budget: budget(),
    })
    .unwrap();
    assert_ne!(base.sha256().unwrap(), changed.sha256().unwrap());
    assert_ne!(
        base.intent.task_revisions[1].sha256,
        changed.intent.task_revisions[1].sha256
    );
    assert!(changed.grants_path("docs/extra.md"));
}

#[test]
fn a_legacy_mandate_records_the_approval_without_an_envelope() {
    let mandate = issue_with(Enforcement::NotAppliedLegacy, None);
    assert!(mandate.scope.grants.is_empty());
    assert_eq!(
        check_write(Some(&mandate), WriteSource::PipelineStage, "anything.rs"),
        WriteVerdict::NotChecked
    );
    assert!(check_transport(&mandate, "backend_agent", "cli", Some("codex")).is_ok());
    assert!(check_checks(&mandate, |_| true, &[]).is_ok());
}

#[test]
fn a_reissue_chains_to_the_previous_revision_and_drops_its_amendments() {
    let first = issue();
    let mut amended = first.clone();
    amended.revision = 2;
    amended.cause = MandateCause::ScopeGrant;
    amended.scope.grants.push(WriteGrant {
        path: "src/granted.rs".into(),
        origins: vec![WriteClass::Amendment],
        unit_key: Some("app_code".into()),
        amendment_revision: Some(2),
    });
    let amended_sha = amended.sha256().unwrap();
    let reissued = issue_with(
        Enforcement::EnforcedV4,
        Some((&amended, amended_sha.as_str())),
    );
    assert_eq!(reissued.revision, 3);
    assert_eq!(reissued.cause, MandateCause::Reissued);
    assert_eq!(
        reissued.previous_sha256.as_deref(),
        Some(amended_sha.as_str())
    );
    assert!(!reissued.grants_path("src/granted.rs"));
}

#[test]
fn a_transport_fallback_changes_only_that_role_and_chains() {
    let first = issue();
    let first_sha = first.sha256().unwrap();
    let next = amend_transport(
        &first,
        first_sha.as_str(),
        TransportCapability {
            role: "test_agent".into(),
            transport: "api".into(),
            backend: None,
        },
        issued_by("cp-fallback"),
    );
    assert_eq!(next.revision, 2);
    assert_eq!(next.cause, MandateCause::TransportFallback);
    assert_eq!(next.previous_sha256.as_deref(), Some(first_sha.as_str()));
    assert_eq!(next.issued_by.checkpoint_id, "cp-fallback");
    assert!(check_transport(&next, "test_agent", "api", None).is_ok());
    assert!(check_transport(&next, "test_agent", "cli", Some("claude")).is_err());
    assert_eq!(next.scope, first.scope);
    assert_eq!(next.budget, first.budget);
    assert_eq!(next.capabilities.transports.len(), 2);
}

#[test]
fn writes_are_checked_only_for_stage_output_under_an_enforced_mandate() {
    let mandate = issue();
    assert_eq!(
        check_write(Some(&mandate), WriteSource::PipelineStage, "src/auth.rs"),
        WriteVerdict::Allowed
    );
    match check_write(Some(&mandate), WriteSource::PipelineStage, "Cargo.toml") {
        WriteVerdict::Refused(reason) => {
            assert!(reason.starts_with("mandate_scope_violation:"), "{reason}");
            assert!(reason.contains("`Cargo.toml`"));
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    for source in WriteSource::ALL {
        if source != WriteSource::PipelineStage {
            assert_eq!(
                check_write(Some(&mandate), source, "Cargo.toml"),
                WriteVerdict::NotChecked,
                "{source:?} has its own approval"
            );
        }
    }
    assert_eq!(
        check_write(None, WriteSource::PipelineStage, "Cargo.toml"),
        WriteVerdict::NotChecked,
        "a pulsation from before the mandate is not checked"
    );
}

/// Property over generated batches: every path a task names is granted, a
/// path no source names is refused, and the digest depends only on the
/// inputs. A fixed linear congruential sequence keeps the cases reproducible.
#[test]
fn generated_envelopes_grant_exactly_the_named_paths() {
    let mut state: u64 = 0x5eed_1234;
    let mut next = |bound: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % bound
    };
    let extensions = ["rs", "ts", "md", "toml", "py"];
    for case in 0..200 {
        let mut manifest = manifest();
        manifest.features[0].brief.sources.clear();
        let mut named = Vec::new();
        for task in &mut manifest.tasks {
            let count = 1 + next(4) as usize;
            let mut description = String::from("Touch");
            for _ in 0..count {
                let path = format!(
                    "src/m{}/f{}.{}",
                    next(9),
                    next(50),
                    extensions[next(extensions.len() as u64) as usize]
                );
                description.push(' ');
                description.push_str(&path);
                named.push(path);
            }
            task.draft.description = description;
        }
        let inputs = |manifest: &BatchManifest| {
            derive_issued(&IssueInputs {
                pulsation_id: "pulse",
                project_id: "project",
                enforcement: Enforcement::EnforcedV4,
                title: "Generated",
                issued_by: issued_by("cp"),
                previous: None,
                manifest: Some(manifest),
                clarification_paths: &[],
                specification: None,
                accepted_artifact_sha256s: &[],
                review_policy_snapshot_sha256: None,
                project_sensitive_sha256: None,
                transports: transports(),
                checks: checks(),
                budget: budget(),
            })
            .unwrap()
        };
        let mandate = inputs(&manifest);
        for path in &named {
            assert_eq!(
                check_write(Some(&mandate), WriteSource::PipelineStage, path),
                WriteVerdict::Allowed,
                "case {case}: `{path}`"
            );
        }
        let outside = format!("lib/injected_{case}.rs");
        assert!(
            matches!(
                check_write(Some(&mandate), WriteSource::PipelineStage, &outside),
                WriteVerdict::Refused(_)
            ),
            "case {case}: an injected path is refused"
        );
        assert_eq!(
            mandate.sha256().unwrap(),
            inputs(&manifest).sha256().unwrap(),
            "case {case}: deterministic"
        );
    }
}

/// The batch limits: 64 tasks naming 256 paths, one of them 260 characters
/// long, all granted at once; a dot-segment path is never in the envelope.
#[test]
fn a_batch_at_the_limits_grants_every_path_it_names() {
    let mut manifest = manifest();
    let template = manifest.tasks[0].clone();
    let long = format!("src/{}/deep.rs", "segment_".repeat(31));
    assert_eq!(long.chars().count(), 260);
    manifest.tasks = (0..64)
        .map(|index| {
            let mut task = template.clone();
            task.id = format!("t-{index:02}");
            task.draft.description = format!(
                "Touch src/a{index}.rs src/b{index}.ts docs/c{index}.md config/d{index}.toml"
            );
            if index == 0 {
                task.draft.description.push_str(&format!(" {long}"));
            }
            task
        })
        .collect();
    let started = std::time::Instant::now();
    let mandate = derive_issued(&IssueInputs {
        pulsation_id: "pulse",
        project_id: "project",
        enforcement: Enforcement::EnforcedV4,
        title: "Limits",
        issued_by: issued_by("cp"),
        previous: None,
        manifest: Some(&manifest),
        clarification_paths: &[],
        specification: None,
        accepted_artifact_sha256s: &[],
        review_policy_snapshot_sha256: None,
        project_sensitive_sha256: None,
        transports: transports(),
        checks: checks(),
        budget: budget(),
    })
    .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(mandate.intent.task_revisions.len(), 64);
    for index in 0..64 {
        for path in [
            format!("src/a{index}.rs"),
            format!("src/b{index}.ts"),
            format!("docs/c{index}.md"),
            format!("config/d{index}.toml"),
        ] {
            assert!(mandate.grants_path(&path), "`{path}`");
        }
    }
    assert!(mandate.grants_path(&long), "the 260-character path");
    assert!(
        mandate.scope.grants.len() >= 257,
        "{}",
        mandate.scope.grants.len()
    );
    assert!(
        matches!(
            check_write(Some(&mandate), WriteSource::PipelineStage, "src/../a0.rs"),
            WriteVerdict::Refused(_)
        ),
        "a dot segment never matches a grant"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "deriving 257 grants took {elapsed:?}"
    );
}

#[test]
fn every_write_source_is_classified() {
    let checked = WriteSource::ALL
        .iter()
        .filter(|source| source.checked_by_mandate())
        .collect::<Vec<_>>();
    assert_eq!(checked, vec![&WriteSource::PipelineStage]);
    assert_eq!(
        WriteSource::of_publication("main", "code"),
        WriteSource::PipelineStage
    );
    assert_eq!(
        WriteSource::of_publication("background", "component"),
        WriteSource::BackgroundComponent
    );
    assert_eq!(
        WriteSource::of_publication("background", "learning_report"),
        WriteSource::BackgroundLearning
    );
    assert_eq!(
        WriteSource::of_publication("background", "knowledge_graph_patch"),
        WriteSource::BackgroundLearning
    );
}

#[test]
fn a_role_must_keep_its_approved_transport() {
    let mandate = issue();
    assert!(check_transport(&mandate, "test_agent", "cli", Some("claude")).is_ok());
    assert!(check_transport(&mandate, "backend_agent", "api", None).is_ok());
    let changed = check_transport(&mandate, "test_agent", "cli", Some("codex")).unwrap_err();
    assert!(changed.starts_with("mandate_capability_mismatch:"));
    assert!(changed.contains("cli/claude") && changed.contains("cli/codex"));
    assert!(
        check_transport(&mandate, "integrator", "api", None).is_err(),
        "a role the approval did not record fails closed"
    );
}

#[test]
fn an_executor_must_keep_its_approved_policy() {
    let mut mandate = issue();
    for kind in ExecutorKind::ALL {
        assert!(check_executor(&mandate, kind).is_ok());
    }
    mandate.capabilities.executors[0]
        .edit_operations
        .push(crate::orchestration::composition::policy::EditOp::ReplaceFile);
    let kind = mandate.capabilities.executors[0].kind;
    let error = check_executor(&mandate, kind).unwrap_err();
    assert!(error.starts_with("mandate_capability_mismatch:"), "{error}");
}

#[test]
fn a_stage_runs_exactly_the_approved_checks_that_apply_to_it() {
    let mandate = issue();
    let code = |kind: CheckKind| matches!(kind, CheckKind::Compile | CheckKind::Lint);
    let compile = checks()
        .into_iter()
        .find(|c| c.check_ref == "check_1")
        .unwrap();
    assert!(check_checks(&mandate, code, std::slice::from_ref(&compile)).is_ok());
    let mut edited = compile.clone();
    edited.profile_sha256 = "f".repeat(64);
    let error = check_checks(&mandate, code, &[edited]).unwrap_err();
    assert!(error.contains("`check_1`"), "{error}");
    assert!(
        check_checks(&mandate, code, &[]).is_err(),
        "an approved check cannot be disabled"
    );
    let lint = CheckCapability {
        check_ref: "check_9".into(),
        kind: CheckKind::Lint,
        profile_sha256: "9".repeat(64),
        required: false,
    };
    let error = check_checks(&mandate, code, &[compile, lint]).unwrap_err();
    assert!(
        error.contains("`check_9`"),
        "a new check needs approval: {error}"
    );
}

#[test]
fn receipts_are_canonical_and_keep_only_digests_of_text() {
    let receipt = ApprovalReceiptV1 {
        schema_version: RECEIPT_SCHEMA_VERSION,
        checkpoint_id: "cp".into(),
        pulsation_id: "pulse".into(),
        purpose: "artifact_review".into(),
        stage: "Integrator".into(),
        decision: "approved".into(),
        state_version: 3,
        input_revision: 1,
        decided_at: "2026-10-26T00:00:00Z".into(),
        reviewed_artifacts: vec![ReviewedArtifact {
            artifact_id: "a1".into(),
            sha256: sha(),
        }],
        confirmed_paths: vec![],
        mandate_revision: Some(1),
        acknowledgements: vec![ReceiptAcknowledgement {
            kind: AcknowledgementKind::Risk,
            reason_sha256: text_sha256("Accepted on purpose."),
        }],
        feedback_sha256: None,
    };
    let bytes = receipt.canonical_bytes().unwrap();
    assert_eq!(bytes, receipt.clone().canonical_bytes().unwrap());
    assert!(!bytes.contains("Accepted on purpose"));
    assert!(bytes.starts_with("{\"acknowledgements\":"));
}

#[test]
fn a_set_digest_ignores_order_and_duplicates() {
    let a = digest_of_set(&["x".into(), "y".into()]).unwrap();
    assert_eq!(
        a,
        digest_of_set(&["y".into(), "x".into(), "y".into()]).unwrap()
    );
    assert_ne!(a, digest_of_set(&["x".into()]).unwrap());
}
