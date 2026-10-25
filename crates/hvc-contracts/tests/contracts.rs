#![allow(
    clippy::unwrap_used,
    reason = "Contract fixtures use known valid values and assert their exact representation."
)]

use std::time::{Duration, Instant};

use hvc_contracts::{
    AgentExecutionTarget, AgentRole, AgentTimeoutPolicy, CliTimeoutBudget, PulsationStage,
    ResponseFormat, STAGES, TimeoutReason, TokenCounts, UnitContextScope, UnitDispatch,
    UsageTransport, canonical_stage, parse_stage, stage_role,
};
use serde_json::json;

#[test]
fn stage_vocabulary_preserves_database_names_roles_and_wire_values() {
    let expected = [
        (PulsationStage::Intake, "Intake", AgentRole::IntakeRouter),
        (
            PulsationStage::Specification,
            "SPECIFICATION",
            AgentRole::TaskSpecifier,
        ),
        (
            PulsationStage::Contract,
            "Contract",
            AgentRole::ContractAgent,
        ),
        (PulsationStage::Code, "Code", AgentRole::BackendAgent),
        (PulsationStage::Test, "Test", AgentRole::TestAgent),
        (
            PulsationStage::Integrator,
            "Integrator",
            AgentRole::Integrator,
        ),
    ];
    assert_eq!(STAGES, expected);
    for (stage, name, role) in expected {
        assert_eq!(stage.as_db_value(), name);
        assert_eq!(PulsationStage::from_db_value(name), Some(stage));
        assert_eq!(parse_stage(name), Some(stage));
        assert_eq!(stage_role(name), Some(role));
        assert_eq!(AgentRole::for_stage(name), role);
        assert_eq!(serde_json::to_value(stage).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<PulsationStage>(json!(name)).unwrap(),
            stage
        );
    }
}

#[test]
fn historical_stage_alias_and_unknown_fallback_do_not_expand_the_wire_contract() {
    assert_eq!(canonical_stage("Specification"), "SPECIFICATION");
    assert_eq!(
        parse_stage("Specification"),
        Some(PulsationStage::Specification)
    );
    assert_eq!(
        AgentRole::for_stage("Specification"),
        AgentRole::TaskSpecifier
    );
    assert!(serde_json::from_value::<PulsationStage>(json!("Specification")).is_err());
    for unknown in ["", "specification", " SPECIFICATION ", "unknown"] {
        assert_eq!(parse_stage(unknown), None);
        assert_eq!(stage_role(unknown), None);
        assert_eq!(AgentRole::for_stage(unknown), AgentRole::TaskSpecifier);
        assert!(serde_json::from_value::<PulsationStage>(json!(unknown)).is_err());
    }
}

#[test]
fn stage_order_and_checkpoint_boundaries_are_unchanged() {
    let expected = [
        (PulsationStage::Intake, false),
        (PulsationStage::Specification, true),
        (PulsationStage::Contract, true),
        (PulsationStage::Code, false),
        (PulsationStage::Test, true),
        (PulsationStage::Integrator, true),
    ];
    for (index, (stage, checkpoint)) in expected.iter().enumerate() {
        assert_eq!(stage.requires_checkpoint(), *checkpoint);
        assert_eq!(stage.next(), expected.get(index + 1).map(|(next, _)| *next));
    }
}

#[test]
fn every_role_keeps_its_persisted_identifier() {
    let identifiers = [
        "intake_router",
        "task_specifier",
        "contract_agent",
        "backend_agent",
        "test_agent",
        "integrator",
        "learning_agent",
        "feature_agent",
        "visualizer",
        "notes_scribe",
        "documentation_reconciler",
    ];
    assert_eq!(AgentRole::ALL.len(), identifiers.len());
    for (role, identifier) in AgentRole::ALL.into_iter().zip(identifiers) {
        assert_eq!(role.as_str(), identifier);
        assert_eq!(AgentRole::parse(&format!(" {identifier} ")), Some(role));
        assert_eq!(serde_json::to_value(role).unwrap(), json!(identifier));
        assert_eq!(
            serde_json::from_value::<AgentRole>(json!(identifier)).unwrap(),
            role
        );
    }
    assert_eq!(AgentRole::parse("unknown"), None);
}

#[test]
fn execution_targets_keep_distinct_lifecycle_and_serialization_keys() {
    let canonical = AgentExecutionTarget::canonical(AgentRole::BackendAgent);
    let ephemeral = AgentExecutionTarget::ephemeral("unit-1", AgentRole::BackendAgent);
    let free_form = AgentExecutionTarget::free_form("slot-1");
    assert_eq!(canonical.stable_key(), "canonical:backend_agent");
    assert_eq!(ephemeral.stable_key(), "ephemeral:unit-1");
    assert_eq!(free_form.stable_key(), "free_form:slot-1");
    assert_eq!(canonical.agent_role(), Some(AgentRole::BackendAgent));
    assert_eq!(ephemeral.agent_role(), Some(AgentRole::BackendAgent));
    assert_eq!(free_form.agent_role(), None);
    assert_eq!(canonical.free_form_slot_id(), None);
    assert_eq!(ephemeral.free_form_slot_id(), None);
    assert_eq!(free_form.free_form_slot_id(), Some("slot-1"));
    let expected = [
        (
            canonical,
            json!({"kind": "canonical", "agent_role": "backend_agent"}),
        ),
        (
            ephemeral,
            json!({"kind": "ephemeral", "slot_id": "unit-1", "agent_role": "backend_agent"}),
        ),
        (free_form, json!({"kind": "free_form", "slot_id": "slot-1"})),
    ];
    for (target, wire) in expected {
        assert_eq!(serde_json::to_value(&target).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<AgentExecutionTarget>(wire).unwrap(),
            target
        );
    }
}

#[test]
fn response_formats_preserve_tagged_schema_content() {
    let formats = [
        (ResponseFormat::Text, json!({"type": "text"}), false),
        (
            ResponseFormat::JsonObject,
            json!({"type": "json_object"}),
            true,
        ),
        (
            ResponseFormat::JsonSchema {
                name: "fixture".into(),
                schema: json!({"type": "object", "additionalProperties": false}),
            },
            json!({
                "type": "json_schema", "name": "fixture",
                "schema": {"type": "object", "additionalProperties": false}
            }),
            true,
        ),
    ];
    for (format, wire, structured) in formats {
        assert_eq!(format.requires_structured_output(), structured);
        assert_eq!(serde_json::to_value(&format).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ResponseFormat>(wire).unwrap(),
            format
        );
    }
}

#[test]
fn unit_dispatch_preserves_frozen_profile_attribution_fields() {
    let unit = UnitDispatch {
        stage_execution_id: "stage-1".into(),
        unit_id: "unit-1".into(),
        attempt_id: "attempt-1".into(),
        executor_kind: "implement_batch".into(),
        frozen_transport: "cli_process".into(),
        frozen_backend: Some("codex".into()),
        frozen_model: "fixture-model".into(),
        context_scope: UnitContextScope::Workspace,
    };
    let wire = json!({
        "stageExecutionId": "stage-1", "unitId": "unit-1", "attemptId": "attempt-1",
        "executorKind": "implement_batch", "frozenTransport": "cli_process",
        "frozenBackend": "codex", "frozenModel": "fixture-model",
        "contextScope": "workspace"
    });
    assert_eq!(serde_json::to_value(&unit).unwrap(), wire);
    assert_eq!(serde_json::from_value::<UnitDispatch>(wire).unwrap(), unit);
    // A request written before ADR-032 keeps the workspace scope.
    let earlier = json!({
        "stageExecutionId": "stage-1", "unitId": "unit-1", "attemptId": "attempt-1",
        "executorKind": "implement_batch", "frozenTransport": "cli_process",
        "frozenBackend": "codex", "frozenModel": "fixture-model"
    });
    assert_eq!(
        serde_json::from_value::<UnitDispatch>(earlier).unwrap(),
        unit
    );
    let reviewer = json!({
        "stageExecutionId": "stage-1", "unitId": "unit-1", "attemptId": "attempt-1",
        "executorKind": "integration_reviewer", "frozenTransport": "cli",
        "frozenBackend": "claude", "frozenModel": "fixture-model",
        "contextScope": "host_context_only"
    });
    assert_eq!(
        serde_json::from_value::<UnitDispatch>(reviewer)
            .unwrap()
            .context_scope,
        UnitContextScope::HostContextOnly
    );
}

#[test]
fn every_composed_specialty_has_one_required_context_scope() {
    // ADR-034 T1: the reviewer is the only specialty confined to its prompt
    // until the T1 measurement moves another one; every other composed
    // specialty reads the sanitized workspace snapshot.
    for kind in [
        "intake_classifier",
        "specification_author",
        "contract_author",
        "code_implementer",
        "code_repairer",
        "test_author",
        "test_repairer",
    ] {
        assert_eq!(
            UnitContextScope::required_for(kind),
            Some(UnitContextScope::Workspace),
            "{kind}"
        );
    }
    assert_eq!(
        UnitContextScope::required_for("integration_reviewer"),
        Some(UnitContextScope::HostContextOnly)
    );
    // Boundary: names that are not composed specialties have no scope, so the
    // transport refuses them instead of guessing one.
    for unknown in [
        "",
        "implement_batch",
        "Integration_Reviewer",
        "integration_reviewer ",
    ] {
        assert_eq!(UnitContextScope::required_for(unknown), None, "{unknown:?}");
    }
}

#[test]
fn role_timeout_budgets_keep_their_existing_absolute_limits() {
    let expected = [
        (AgentRole::FeatureAgent, 300_000, 900_000),
        (AgentRole::IntakeRouter, 180_000, 600_000),
        (AgentRole::TaskSpecifier, 240_000, 900_000),
        (AgentRole::ContractAgent, 240_000, 900_000),
        (AgentRole::BackendAgent, 360_000, 1_200_000),
        (AgentRole::TestAgent, 360_000, 1_200_000),
        (AgentRole::Integrator, 240_000, 900_000),
        (AgentRole::LearningAgent, 240_000, 900_000),
        (AgentRole::Visualizer, 120_000, 300_000),
        (AgentRole::NotesScribe, 180_000, 600_000),
        (AgentRole::DocumentationReconciler, 240_000, 600_000),
    ];
    for (role, initial, ceiling) in expected {
        let policy = AgentTimeoutPolicy::for_role(role);
        let silence = if role == AgentRole::Visualizer {
            60_000
        } else {
            90_000
        };
        assert_eq!(policy.base_timeout_ms, initial);
        assert_eq!(policy.max_timeout_ms, ceiling);
        assert_eq!(policy.inactivity_window_ms, silence);
        assert_eq!(policy.reasoning_silence_window_ms, silence * 3);
        assert_eq!(policy.extension_increment_ms, 30_000);
        assert_eq!(
            serde_json::from_value::<AgentTimeoutPolicy>(serde_json::to_value(policy).unwrap())
                .unwrap(),
            policy
        );
    }
    assert_eq!(AgentTimeoutPolicy::companion().base_timeout_ms, 120_000);
    assert_eq!(AgentTimeoutPolicy::companion().max_timeout_ms, 600_000);
    assert_eq!(AgentTimeoutPolicy::manual().base_timeout_ms, 1_800_000);
    assert_eq!(AgentTimeoutPolicy::manual().max_timeout_ms, 3_600_000);
}

#[test]
fn cli_budget_clamps_requested_initial_time_without_shortening_absolute_deadline() {
    let before = Instant::now();
    let budget = CliTimeoutBudget::for_role(AgentRole::TaskSpecifier, 180_000);
    let after = Instant::now();
    assert_eq!(budget.policy.base_timeout_ms, 240_000);
    assert!(budget.deadline >= before + Duration::from_secs(900));
    assert!(budget.deadline <= after + Duration::from_secs(900));
    let bounded = CliTimeoutBudget::for_role(AgentRole::IntakeRouter, u64::MAX);
    assert_eq!(
        bounded.policy.base_timeout_ms,
        bounded.policy.max_timeout_ms
    );
    assert_eq!(
        AgentTimeoutPolicy::new(1, u64::MAX, 2, 1).reasoning_silence_window_ms,
        u64::MAX
    );
    assert_eq!(TimeoutReason::Inactivity.as_str(), "cli_inactivity_timeout");
    assert_eq!(TimeoutReason::MaxCeiling.as_str(), "cli_timeout");
}

#[test]
fn usage_contracts_preserve_unknown_counters_and_transport_values() {
    let unknown = TokenCounts::default();
    assert_eq!(
        serde_json::to_value(&unknown).unwrap(),
        json!({
            "inputTokens": null, "outputTokens": null, "totalTokens": null,
            "cacheReadTokens": null, "cacheWriteTokens": null, "reasoningTokens": null
        })
    );
    let counts = TokenCounts {
        input_tokens: Some(12),
        output_tokens: Some(7),
        total_tokens: Some(19),
        cache_read_tokens: Some(4),
        cache_write_tokens: None,
        reasoning_tokens: Some(2),
    };
    assert_eq!(
        serde_json::from_value::<TokenCounts>(serde_json::to_value(&counts).unwrap()).unwrap(),
        counts
    );
    for (transport, identifier) in [(UsageTransport::Api, "api"), (UsageTransport::Cli, "cli")] {
        assert_eq!(transport.as_str(), identifier);
        assert_eq!(serde_json::to_value(transport).unwrap(), json!(identifier));
        assert_eq!(
            serde_json::from_value::<UsageTransport>(json!(identifier)).unwrap(),
            transport
        );
    }
}
