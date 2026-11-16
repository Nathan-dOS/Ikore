use super::*;

/// Every snapshot keeps the contract: four layers in order, at most
/// [`MAX_LAYER_ITEMS`] items, and a reason exactly when the state is flagged.
fn derive(inputs: &LayerInputs) -> PulsationLayersSnapshot {
    let snapshot = derive_layers(inputs);
    assert_eq!(snapshot.layers_policy_version, LAYERS_POLICY_VERSION);
    assert_eq!(
        snapshot
            .layers
            .iter()
            .map(|layer| layer.kind)
            .collect::<Vec<_>>(),
        LayerKind::ALL
    );
    for layer in &snapshot.layers {
        assert!(layer.items.len() <= MAX_LAYER_ITEMS, "{layer:?}");
        assert_eq!(
            layer.state.is_flagged(),
            !layer.reasons.is_empty(),
            "a flagged layer explains itself and only then: {layer:?}"
        );
        assert!(!layer.headline.is_empty());
    }
    snapshot
}

fn state(inputs: &LayerInputs, kind: LayerKind) -> LayerState {
    derive(inputs).layer(kind).unwrap().state
}

fn composed() -> LayerInputs {
    LayerInputs {
        composed: true,
        past_specification: true,
        mandate: Some(MandateFact {
            revision: 1,
            cause: MandateCause::Issued,
            granted: 3,
            transport_fallback: false,
        }),
        runs: vec![run(
            "code_implementer",
            Some("gpt-4.1-mini"),
            Some("gpt-4.1-mini"),
        )],
        usage: UsageFact {
            complete: 4,
            ..UsageFact::default()
        },
        criteria: Some(CriteriaFact {
            total: 2,
            assigned: 2,
            validated: 2,
            without_check: Vec::new(),
        }),
        risk: Some(RiskFact {
            level: Risk::Medium,
            reasons: vec!["note risk `docs/auth.md` (medium)".into()],
        }),
        rounds: Some(RoundsFact {
            rounds: 0,
            threshold: 3,
        }),
        ..LayerInputs::default()
    }
}

fn run(actor: &str, requested: Option<&str>, observed: Option<&str>) -> RunFact {
    RunFact {
        actor: actor.into(),
        transport: "api".into(),
        provider: Some("openai".into()),
        requested_model: requested.map(Into::into),
        observed_model: observed.map(Into::into),
        adapter_version: None,
        completed: true,
    }
}

fn check(check_ref: &str, required: bool, status: CheckStatus) -> HostCheckFact {
    HostCheckFact {
        check_ref: check_ref.into(),
        required,
        status,
    }
}

fn budget(active: bool, left: u64, limit: u64) -> BudgetFact {
    BudgetFact {
        stage: "Code".into(),
        active,
        exhausted: false,
        unit_waiting: false,
        extra_pools: vec![PoolFact {
            name: "output tokens".into(),
            left,
            limit,
        }],
    }
}

#[test]
fn a_composed_pulsation_without_findings_is_clear() {
    let snapshot = derive(&composed());
    for layer in &snapshot.layers {
        assert_eq!(layer.state, LayerState::Clear, "{layer:?}");
    }
    assert!(snapshot.worst_flagged().is_none());
}

#[test]
fn a_new_pulsation_is_unknown_and_flags_nothing() {
    let snapshot = derive(&LayerInputs {
        composed: true,
        ..LayerInputs::default()
    });
    for layer in &snapshot.layers {
        assert_eq!(layer.state, LayerState::Unknown, "{layer:?}");
        assert!(layer.items.is_empty());
    }
    assert!(snapshot.worst_flagged().is_none());
}

#[test]
fn a_legacy_pulsation_marks_scope_and_intent_not_applicable() {
    let snapshot = derive(&LayerInputs {
        composed: false,
        past_specification: true,
        runs: vec![run("backend_agent", Some("gpt-4.1"), Some("gpt-4.1"))],
        usage: UsageFact {
            complete: 1,
            ..UsageFact::default()
        },
        redactions: 2,
        ..LayerInputs::default()
    });
    let layer = |kind| snapshot.layer(kind).unwrap();
    assert_eq!(layer(LayerKind::Identity).state, LayerState::Clear);
    assert!(
        layer(LayerKind::Identity)
            .headline
            .ends_with("legacy pipeline"),
        "{:?}",
        layer(LayerKind::Identity)
    );
    assert_eq!(layer(LayerKind::Scope).state, LayerState::NotApplicable);
    assert_eq!(
        layer(LayerKind::Scope).headline,
        "Not enforced (legacy pipeline)."
    );
    assert_eq!(layer(LayerKind::Guardrails).state, LayerState::Clear);
    assert_eq!(layer(LayerKind::Intent).state, LayerState::NotApplicable);
    assert_eq!(
        layer(LayerKind::Intent).headline,
        "Criteria states unavailable (legacy pipeline)."
    );
}

#[test]
fn identity_flags_a_model_other_than_the_request_or_none_reported() {
    let with = |runs: Vec<RunFact>| LayerInputs { runs, ..composed() };
    let mismatch = derive(&with(vec![run(
        "code_implementer",
        Some("gpt-4.1-mini"),
        Some("gpt-4.1"),
    )]));
    let identity = mismatch.layer(LayerKind::Identity).unwrap();
    assert_eq!(identity.state, LayerState::Attention);
    assert_eq!(
        identity.reasons,
        ["`code_implementer` ran `gpt-4.1`, not the requested `gpt-4.1-mini`."]
    );
    assert_eq!(identity.items[0].state, LayerState::Attention);
    assert_eq!(
        state(
            &with(vec![run("test_author", Some("gpt-4.1"), None)]),
            LayerKind::Identity
        ),
        LayerState::Attention,
        "a completed run that reported no model"
    );
    let mut running = run("test_author", Some("gpt-4.1"), None);
    running.completed = false;
    assert_eq!(
        state(&with(vec![running]), LayerKind::Identity),
        LayerState::Clear,
        "a run still in flight has nothing to report yet"
    );
    assert_eq!(
        state(
            &with(vec![run("test_author", Some("default"), Some("gpt-4.1"))]),
            LayerKind::Identity
        ),
        LayerState::Clear,
        "a dynamic alias cannot be compared"
    );
    let mut claude = run(
        "integration_reviewer",
        Some("sonnet"),
        Some("claude-sonnet-5"),
    );
    claude.provider = Some("claude".into());
    assert_eq!(
        state(&with(vec![claude]), LayerKind::Identity),
        LayerState::Clear,
        "a family alias matches its family"
    );
}

#[test]
fn identity_groups_runs_and_never_raises_an_issue() {
    let inputs = LayerInputs {
        runs: vec![
            run("code_implementer", Some("a"), Some("b")),
            run("code_implementer", Some("a"), Some("b")),
            run("test_author", Some("a"), Some("a")),
        ],
        approvals: vec![ApprovalFact {
            decided_at: "2026-10-26T12:00:00Z".into(),
            stage: "SPECIFICATION".into(),
            purpose: "artifact_review".into(),
            decision: "approved".into(),
        }],
        ..composed()
    };
    let snapshot = derive(&inputs);
    let identity = snapshot.layer(LayerKind::Identity).unwrap();
    assert_eq!(identity.state, LayerState::Attention);
    assert!(
        identity.items[0].value.ends_with("· 2 run(s)"),
        "{identity:?}"
    );
    assert!(
        identity
            .items
            .iter()
            .any(|item| item.label == "SPECIFICATION artifact review"
                && item.value == "approved · 2026-10-26T12:00:00Z")
    );
    assert_eq!(
        identity.headline,
        "3 run(s) on 2 model(s) · mandate revision 1"
    );
}

#[test]
fn identity_flags_a_transport_fallback_and_a_missing_mandate() {
    let mut fallback = composed();
    fallback.mandate.as_mut().unwrap().transport_fallback = true;
    assert_eq!(state(&fallback, LayerKind::Identity), LayerState::Attention);
    let missing = LayerInputs {
        mandate: None,
        ..composed()
    };
    assert_eq!(state(&missing, LayerKind::Identity), LayerState::Attention);
    let before = LayerInputs {
        mandate: None,
        past_specification: false,
        ..composed()
    };
    assert_eq!(
        state(&before, LayerKind::Identity),
        LayerState::Clear,
        "before the approval no mandate is expected"
    );
}

#[test]
fn scope_follows_the_mandate_and_what_was_written() {
    let base = composed();
    assert_eq!(state(&base, LayerKind::Scope), LayerState::Clear);
    let sensitive = LayerInputs {
        sensitive_granted: vec!["src/auth.rs".into()],
        ..composed()
    };
    let snapshot = derive(&sensitive);
    let scope = snapshot.layer(LayerKind::Scope).unwrap();
    assert_eq!(scope.state, LayerState::Attention);
    assert_eq!(
        scope.reasons,
        ["1 sensitive path(s) in the write envelope."]
    );
    let written = LayerInputs {
        publications: vec![
            PublicationFact {
                path: "src/auth.rs".into(),
                state: "applied".into(),
                sensitive: true,
            },
            PublicationFact {
                path: "src/lib.rs".into(),
                state: "applied".into(),
                sensitive: false,
            },
        ],
        ..composed()
    };
    let snapshot = derive(&written);
    let scope = snapshot.layer(LayerKind::Scope).unwrap();
    assert_eq!(scope.state, LayerState::Attention);
    assert_eq!(scope.headline, "3 granted · 2 written · 0 refused");
    let conflict = LayerInputs {
        publications: vec![PublicationFact {
            path: "src/lib.rs".into(),
            state: "conflict".into(),
            sensitive: false,
        }],
        ..composed()
    };
    assert_eq!(state(&conflict, LayerKind::Scope), LayerState::Issue);
    let refused = LayerInputs {
        refusals: vec![
            RefusalFact {
                rule: "scope_violation".into(),
                path: Some("Cargo.toml".into()),
                occurrences: 2,
            },
            RefusalFact {
                rule: "capability_mismatch".into(),
                path: None,
                occurrences: 1,
            },
        ],
        ..composed()
    };
    let snapshot = derive(&refused);
    let scope = snapshot.layer(LayerKind::Scope).unwrap();
    assert_eq!(scope.state, LayerState::Issue);
    assert!(
        scope
            .items
            .iter()
            .any(|item| item.label == "Cargo.toml" && item.value == "write refused · 2 time(s)")
    );
    assert!(
        scope
            .items
            .iter()
            .any(|item| item.label == "Dispatch" && item.value == "dispatch refused · 1 time(s)")
    );
    let corrected = LayerInputs {
        ownership_violations: 1,
        ..composed()
    };
    assert_eq!(state(&corrected, LayerKind::Scope), LayerState::Attention);
}

#[test]
fn scope_without_a_mandate_says_why() {
    let before = LayerInputs {
        mandate: None,
        past_specification: false,
        ..composed()
    };
    let snapshot = derive(&before);
    assert_eq!(
        snapshot.layer(LayerKind::Scope).unwrap().state,
        LayerState::Unknown
    );
    let upgraded = LayerInputs {
        mandate: None,
        ..composed()
    };
    let snapshot = derive(&upgraded);
    let scope = snapshot.layer(LayerKind::Scope).unwrap();
    assert_eq!(scope.state, LayerState::NotApplicable);
    assert_eq!(
        scope.headline,
        "Mandate not issued (started before this version)."
    );
    let tampered = LayerInputs {
        mandate: None,
        mandate_integrity_failed: true,
        ..composed()
    };
    assert_eq!(state(&tampered, LayerKind::Scope), LayerState::Issue);
}

#[test]
fn guardrails_flags_an_extra_pool_below_a_quarter_while_the_stage_runs() {
    let with = |budget: BudgetFact| LayerInputs {
        budget: Some(budget),
        ..composed()
    };
    assert_eq!(
        state(&with(budget(true, 25, 100)), LayerKind::Guardrails),
        LayerState::Clear,
        "exactly a quarter left"
    );
    let low = derive(&with(budget(true, 24, 100)));
    let guardrails = low.layer(LayerKind::Guardrails).unwrap();
    assert_eq!(guardrails.state, LayerState::Attention);
    assert_eq!(
        guardrails.reasons,
        ["Less than 25% of the Code extra output tokens is left."]
    );
    assert!(guardrails.headline.contains("24% extra budget left"));
    assert_eq!(
        state(&with(budget(false, 0, 100)), LayerKind::Guardrails),
        LayerState::Clear,
        "a settled execution's budget no longer limits anything"
    );
    assert_eq!(
        state(&with(budget(true, 0, 0)), LayerKind::Guardrails),
        LayerState::Clear,
        "a pool without a limit is not a margin"
    );
    let mut exhausted = budget(true, 30, 100);
    exhausted.exhausted = true;
    assert_eq!(
        state(&with(exhausted.clone()), LayerKind::Guardrails),
        LayerState::Clear,
        "exhausted with nothing left to run"
    );
    exhausted.unit_waiting = true;
    assert_eq!(
        state(&with(exhausted), LayerKind::Guardrails),
        LayerState::Issue
    );
}

#[test]
fn guardrails_rates_required_checks_by_their_result() {
    let with = |checks: Vec<HostCheckFact>| LayerInputs {
        checks,
        ..composed()
    };
    for status in [CheckStatus::Failed, CheckStatus::TimedOut] {
        assert_eq!(
            state(
                &with(vec![check("cargo_test", true, status)]),
                LayerKind::Guardrails
            ),
            LayerState::Issue,
            "{status:?}"
        );
    }
    for status in [
        CheckStatus::Unavailable,
        CheckStatus::NotRun,
        CheckStatus::Cancelled,
        CheckStatus::Error,
    ] {
        assert_eq!(
            state(
                &with(vec![check("cargo_test", true, status)]),
                LayerKind::Guardrails
            ),
            LayerState::Attention,
            "{status:?}"
        );
    }
    for status in [
        CheckStatus::Passed,
        CheckStatus::Pending,
        CheckStatus::Running,
    ] {
        assert_eq!(
            state(
                &with(vec![check("cargo_test", true, status)]),
                LayerKind::Guardrails
            ),
            LayerState::Clear,
            "{status:?}"
        );
    }
    assert_eq!(
        state(
            &with(vec![check("lint", false, CheckStatus::Failed)]),
            LayerKind::Guardrails
        ),
        LayerState::Clear,
        "an optional check informs only"
    );
    let snapshot = derive(&with(vec![
        check("cargo_test", true, CheckStatus::Passed),
        check("clippy", true, CheckStatus::Passed),
    ]));
    assert_eq!(
        snapshot.layer(LayerKind::Guardrails).unwrap().headline,
        "2/2 required check(s) passed"
    );
}

#[test]
fn guardrails_flags_rounds_near_the_threshold_and_partial_usage() {
    let rounds = |rounds, threshold| LayerInputs {
        rounds: Some(RoundsFact { rounds, threshold }),
        ..composed()
    };
    assert_eq!(
        state(&rounds(1, 3), LayerKind::Guardrails),
        LayerState::Clear
    );
    assert_eq!(
        state(&rounds(2, 3), LayerKind::Guardrails),
        LayerState::Attention
    );
    assert_eq!(
        state(&rounds(5, 3), LayerKind::Guardrails),
        LayerState::Attention
    );
    let partial = LayerInputs {
        usage: UsageFact {
            complete: 3,
            partial: 1,
            unavailable: 0,
        },
        ..composed()
    };
    assert_eq!(
        state(&partial, LayerKind::Guardrails),
        LayerState::Attention
    );
}

#[test]
fn intent_follows_risk_coverage_and_verdicts() {
    for (level, expected) in [
        (Risk::Low, LayerState::Clear),
        (Risk::Medium, LayerState::Clear),
        (Risk::High, LayerState::Attention),
        (Risk::Unknown, LayerState::Attention),
    ] {
        let inputs = LayerInputs {
            risk: Some(RiskFact {
                level,
                reasons: Vec::new(),
            }),
            ..composed()
        };
        assert_eq!(state(&inputs, LayerKind::Intent), expected, "{level:?}");
    }
    let row = |kind, state| CoverageRowFact {
        kind,
        subject: "crit_1".into(),
        state,
    };
    for (coverage, expected) in [
        (CoverageState::Executed, LayerState::Clear),
        (CoverageState::NotRun, LayerState::Clear),
        (CoverageState::Unverified, LayerState::Attention),
        (CoverageState::ModelOnly, LayerState::Attention),
    ] {
        let inputs = LayerInputs {
            coverage: vec![row(CoverageRowKind::Criterion, coverage)],
            ..composed()
        };
        assert_eq!(state(&inputs, LayerKind::Intent), expected, "{coverage:?}");
    }
    let contradicted = LayerInputs {
        verdicts: vec![
            CriterionVerdict {
                criterion: "crit_1".into(),
                verdict: Verdict::Supported,
            },
            CriterionVerdict {
                criterion: "crit_2".into(),
                verdict: Verdict::Contradicted,
            },
        ],
        ..composed()
    };
    let snapshot = derive(&contradicted);
    let intent = snapshot.layer(LayerKind::Intent).unwrap();
    assert_eq!(intent.state, LayerState::Issue);
    assert_eq!(
        intent.reasons,
        ["The Integrator found 1 criterion(s) contradicted."]
    );
    assert!(intent.headline.ends_with("1 supported"), "{intent:?}");
    let unnamed = LayerInputs {
        criteria: Some(CriteriaFact {
            total: 2,
            assigned: 1,
            validated: 0,
            without_check: vec!["crit_2".into()],
        }),
        ..composed()
    };
    assert_eq!(state(&unnamed, LayerKind::Intent), LayerState::Attention);
    let unapproved = LayerInputs {
        criteria: None,
        ..composed()
    };
    let snapshot = derive(&unapproved);
    let intent = snapshot.layer(LayerKind::Intent).unwrap();
    assert_eq!(intent.state, LayerState::Unknown);
    assert_eq!(intent.headline, "No approved criteria yet.");
}

#[test]
fn the_same_facts_in_any_order_give_the_same_snapshot() {
    let mut inputs = composed();
    inputs.runs = vec![
        run("code_implementer", Some("a"), Some("b")),
        run("test_author", Some("c"), Some("c")),
        run("code_implementer", Some("a"), Some("a")),
    ];
    inputs.sensitive_granted = vec!["src/b.rs".into(), "src/a.rs".into()];
    inputs.publications = vec![
        PublicationFact {
            path: "src/z.rs".into(),
            state: "applied".into(),
            sensitive: false,
        },
        PublicationFact {
            path: "src/a.rs".into(),
            state: "conflict".into(),
            sensitive: true,
        },
    ];
    inputs.checks = vec![
        check("z_check", true, CheckStatus::Passed),
        check("a_check", true, CheckStatus::NotRun),
    ];
    inputs.tool_policies = vec![
        ("test_author".into(), "read_only_scoped".into()),
        ("code_implementer".into(), "read_only_scoped".into()),
    ];
    inputs.approvals = vec![
        ApprovalFact {
            decided_at: "2026-10-26T13:00:00Z".into(),
            stage: "Contract".into(),
            purpose: "artifact_review".into(),
            decision: "approved".into(),
        },
        ApprovalFact {
            decided_at: "2026-10-26T12:00:00Z".into(),
            stage: "SPECIFICATION".into(),
            purpose: "artifact_review".into(),
            decision: "approved".into(),
        },
    ];
    let first = derive(&inputs);
    let mut shuffled = inputs.clone();
    shuffled.runs.reverse();
    shuffled.sensitive_granted.reverse();
    shuffled.publications.reverse();
    shuffled.checks.reverse();
    shuffled.tool_policies.reverse();
    shuffled.approvals.reverse();
    let second = derive(&shuffled);
    assert_eq!(first, second);
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
}

#[test]
fn a_layer_keeps_fifty_items_and_counts_the_rest() {
    // The batch limits of the mandate: 64 tasks naming 257 paths.
    let started = std::time::Instant::now();
    let inputs = LayerInputs {
        mandate: Some(MandateFact {
            revision: 1,
            cause: MandateCause::Issued,
            granted: 257,
            transport_fallback: false,
        }),
        sensitive_granted: (0..257)
            .map(|index| format!("src/auth_{index:03}.rs"))
            .collect(),
        publications: (0..200)
            .map(|index| PublicationFact {
                path: format!("src/file_{index:03}.rs"),
                state: "applied".into(),
                sensitive: false,
            })
            .collect(),
        criteria: Some(CriteriaFact {
            total: 64,
            assigned: 64,
            validated: 64,
            without_check: (0..64).map(|index| format!("crit_{index:02}")).collect(),
        }),
        ..composed()
    };
    let snapshot = derive(&inputs);
    let elapsed = started.elapsed();
    let scope = snapshot.layer(LayerKind::Scope).unwrap();
    assert_eq!(scope.items.len(), MAX_LAYER_ITEMS);
    assert_eq!(scope.truncated_count, 1 + 257 + 200 - MAX_LAYER_ITEMS);
    assert_eq!(
        scope.items[1].label, "src/auth_000.rs",
        "sorted before the cut"
    );
    let intent = snapshot.layer(LayerKind::Intent).unwrap();
    assert_eq!(intent.items.len() + intent.truncated_count, 3 + 64 + 2);
    assert!(
        elapsed < std::time::Duration::from_millis(50),
        "deriving 64 tasks took {elapsed:?}"
    );
}

#[test]
fn the_worst_flagged_layer_is_the_most_severe_first_in_order() {
    let mut inputs = composed();
    inputs.runs = vec![run("code_implementer", Some("a"), Some("b"))];
    inputs.sensitive_granted = vec!["src/auth.rs".into()];
    let snapshot = derive(&inputs);
    assert_eq!(
        snapshot.worst_flagged().map(|layer| layer.kind),
        Some(LayerKind::Identity),
        "two layers in attention: the first one names the dot"
    );
    inputs.verdicts = vec![CriterionVerdict {
        criterion: "crit_1".into(),
        verdict: Verdict::Contradicted,
    }];
    let snapshot = derive(&inputs);
    assert_eq!(
        snapshot.worst_flagged().map(|layer| layer.kind),
        Some(LayerKind::Intent)
    );
}
