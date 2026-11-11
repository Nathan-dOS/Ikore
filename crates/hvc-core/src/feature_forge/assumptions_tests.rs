use super::*;
use crate::feature_forge::{AssumptionReviewAction, ReviewAssumptionRequest};

#[tokio::test]
async fn forge_assumptions_security_default_cannot_bypass_a_reported_policy_conflict() {
    let f = Fixture::new();
    f.write("knowledge/obsidian/decisions/retention.md", "---\ntype: decision\nstatus: accepted\n---\n# Export retention\nDelete exports after one day.\n");
    let feature = f.feature("Export retention");
    let mut brief = feature.brief.clone();
    brief.assumptions.push(
        serde_json::from_value(json!({
            "id":"SEC1","text":"Keep exports for thirty days.","category":"security",
            "reason":"The exported archive may be needed later."
        }))
        .unwrap(),
    );
    let proposed = f
        .forge()
        .revise(&f.project, &feature.id, feature.revision, brief)
        .unwrap();
    let reply = triage_with_conflicts(
        "approve",
        json!([{
            "decisionRef":"decision_1","severity":"blocking",
            "conflict":"The retention assumption exceeds the project limit."
        }]),
    );
    let gateway = triaged(&f, &proposed, reply).await;
    let prompts = gateway.prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("Delete exports after one day."));
    assert!(prompts[0].contains("Keep exports for thirty days."));
    let stored = f.forge().get(&f.project, &feature.id).unwrap();
    assert_eq!(stored.approved_revision, None);
    assert!(
        f.forge()
            .approve(&f.project, &feature.id, stored.revision)
            .unwrap_err()
            .to_string()
            .contains("preflight_conflict_unresolved")
    );
}

#[tokio::test]
async fn forge_assumptions_question_budget_survives_restart_until_an_explicit_new_round() {
    use crate::feature_forge::{CONTINUE_CLARIFICATION, remaining_questions};
    let f = Fixture::new();
    let feature = f.feature("Export formats");
    for (message, expected, left) in [
        ("Discuss exports", "completed", 2),
        ("Use JSON", "completed", 1),
        ("Use visible records", "completed", 0),
        ("Keep this choice", "failed", 0),
        (CONTINUE_CLARIFICATION, "completed", 2),
    ] {
        let current = f.forge().get(&f.project, &feature.id).unwrap();
        let (runtime, gateway) = planning_runtime(&f, &current);
        gateway.release.add_permits(1);
        runtime
            .start(&f.project, &feature.id, current.revision, &id(), message)
            .unwrap();
        runtime.wait_for_idle().await;
        let history = runtime.history(&f.project, &feature.id).unwrap();
        let last = history.last().unwrap();
        assert_eq!(last.status, expected, "{:?}", last.failure);
        assert_eq!(remaining_questions(&history), left);
        let updated = f.forge().get(&f.project, &feature.id).unwrap();
        assert!(!updated.brief.pending_decisions.is_empty());
        assert_eq!(
            updated.revision,
            current.revision + u64::from(expected == "completed")
        );
    }
}

#[test]
fn forge_assumptions_keep_human_flags_through_retry_restart_and_model_revisions() {
    let f = Fixture::new();
    let feature = f.feature("Export records");
    let mut value = serde_json::to_value(&feature.brief).unwrap();
    value["assumptions"] = json!([{
        "id":"A1","text":"Export visible records.","category":"scope",
        "reason":"The user is viewing a filtered list."
    }]);
    let brief: FeatureBrief = serde_json::from_value(value).unwrap();
    let proposed = f
        .forge()
        .revise(&f.project, &feature.id, feature.revision, brief.clone())
        .unwrap();
    let flag = ReviewAssumptionRequest {
        project_id: f.project.clone(),
        feature_id: feature.id.clone(),
        expected_revision: proposed.revision,
        assumption_id: "A1".into(),
        request_id: id(),
        action: AssumptionReviewAction::Flag,
    };
    let revision = f.forge().review_assumption(&flag).unwrap();
    let reopened =
        FeatureForgeService::new(PersistenceService::initialize(f._data.path()).unwrap());
    assert_eq!(reopened.review_assumption(&flag).unwrap(), revision);
    assert!(reopened.approve(&f.project, &feature.id, revision).is_err());
    assert!(
        reopened
            .revise(&f.project, &feature.id, revision, feature.brief.clone())
            .is_err()
    );
    let carried = reopened
        .revise(&f.project, &feature.id, revision, brief)
        .unwrap();
    assert!(
        reopened
            .approve(&f.project, &feature.id, carried.revision)
            .is_err()
    );
    assert!(!carried.brief.pending_decisions.is_empty());
    let mut answer = ReviewAssumptionRequest {
        action: AssumptionReviewAction::Answer {
            text: "Somente os itens visíveis.".into(),
        },
        request_id: id(),
        ..flag
    };
    assert!(reopened.review_assumption(&answer).is_err());
    answer.expected_revision = carried.revision;
    let answered = reopened.review_assumption(&answer).unwrap();
    assert_eq!(reopened.review_assumption(&answer).unwrap(), answered);
    let approved = reopened.approve(&f.project, &feature.id, answered).unwrap();
    assert!(approved.brief.pending_decisions.is_empty());
    assert_eq!(
        reopened.publication_issues(&f.project).unwrap(),
        Vec::<String>::new()
    );
    let published = std::fs::read_to_string(f.workspace.path().join(format!(
        "knowledge/obsidian/product/features/{}/revision-{}.md",
        feature.id, answered
    )))
    .unwrap();
    assert!(published.contains("Export visible records."));
    assert!(published.contains("Somente os itens visíveis."));
}
