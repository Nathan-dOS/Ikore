use super::*;

#[test]
fn repeated_path_decisions_do_not_duplicate_or_advance_the_revision() {
    let f = Fixture::new();
    let feature = f.approved("Export notes");
    let task = feature_task(&f, &feature);
    f.forge()
        .request_path_clarification(&f.project, &feature.id, &task.draft.title)
        .unwrap();
    let reopened = f.forge().get(&f.project, &feature.id).unwrap();
    f.forge()
        .request_path_clarification(&f.project, &feature.id, &task.draft.title)
        .unwrap();
    let repeated = f.forge().get(&f.project, &feature.id).unwrap();
    assert_eq!(repeated.revision, reopened.revision);
    assert_eq!(
        repeated.brief.pending_decisions,
        reopened.brief.pending_decisions
    );
    assert_eq!(repeated.brief.pending_decisions.len(), 1);
}

#[tokio::test]
async fn asking_for_a_path_starts_the_agent_and_replays_without_reopening() {
    let f = Fixture::new();
    let feature = f.approved("Export notes");
    let task = feature_task(&f, &feature);
    let mut answer = feature.clone();
    answer.brief.tasks[0]
        .description
        .push_str(" Use `src/export.rs` for the export implementation.");
    let gateway = ScriptedGateway::new(vec![discovery_reply(
        &answer,
        "O caminho sugerido é src/export.rs.",
        vec![],
    )]);
    let runtime = runtime_with(&f, gateway.clone());
    let request_id = "explicit-path-request";
    let turn_id = runtime
        .start_path_clarification(&f.project, &feature.id, &task.draft.title, request_id)
        .unwrap();
    let turn = only_turn(&runtime, &f, &feature).await;
    assert_eq!(turn.id, turn_id);
    assert_eq!(turn.status, "completed", "{:?}", turn.error);
    assert!(turn.user_message.contains(&task.draft.title));
    let prompts = gateway.prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("Define the explicit file path(s)"));
    assert!(prompts[0].contains("File placement is the application's responsibility"));
    {
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(
            requests[0].target.agent_role(),
            Some(AgentRole::FeatureAgent)
        );
    }

    let resolved = f.forge().get(&f.project, &feature.id).unwrap();
    assert!(resolved.brief.pending_decisions.is_empty());
    assert!(
        resolved.brief.tasks[0]
            .description
            .contains("src/export.rs")
    );
    assert_eq!(resolved.state, FeatureState::Incubating);
    assert!(!f.workspace.path().join("src/export.rs").exists());
    let pulsations: u64 = f
        .service
        .connection()
        .unwrap()
        .query_row("SELECT count(*) FROM pulsations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pulsations, 0);

    assert_eq!(
        runtime
            .start_path_clarification(&f.project, &feature.id, &task.draft.title, request_id)
            .unwrap(),
        turn_id
    );
    runtime.wait_for_idle().await;
    assert_eq!(gateway.prompts().len(), 1);
    let replayed = f.forge().get(&f.project, &feature.id).unwrap();
    assert_eq!(replayed.revision, resolved.revision);
    assert!(replayed.brief.pending_decisions.is_empty());
    assert!(
        runtime
            .start_path_clarification(&f.project, &feature.id, "A different task", request_id)
            .is_err()
    );
}

#[tokio::test]
async fn asking_for_a_path_on_an_archived_feature_does_not_start_a_turn() {
    let f = Fixture::new();
    let feature = f.feature("Archived export");
    f.forge()
        .archive(&f.project, &feature.id, "Out of scope")
        .unwrap();
    let gateway = ScriptedGateway::new(vec![discovery_reply(&feature, "Unused", vec![])]);
    let runtime = runtime_with(&f, gateway.clone());
    assert!(
        runtime
            .start_path_clarification(&f.project, &feature.id, "Export notes", "archived-path")
            .is_err()
    );
    runtime.wait_for_idle().await;
    assert!(gateway.prompts().is_empty());
    assert!(runtime.history(&f.project, &feature.id).unwrap().is_empty());
}
