use ikore_test_support::pulsation_benchmark::*;

#[test]
fn real_worker_matches_work_and_fences_stale_context_in_all_benchmark_scenarios() {
    for scenario in [
        Scenario::NoQueue,
        Scenario::QueueAbc,
        Scenario::EditWhileWaiting,
    ] {
        let off = run_sample(scenario, false, 1, false);
        let on = run_sample(scenario, true, 1, false);
        assert_eq!(off.fixture_sha256, on.fixture_sha256);
        assert_eq!(off.provider_calls, on.provider_calls);
        assert!(
            on.context_metrics
                .iter()
                .all(|m| m["read_bytes"].as_u64().unwrap() <= 16 * 1024 * 1024)
        );
        assert!(
            on.context_metrics
                .iter()
                .all(|m| m["enumerated_files"] == 128)
        );
        assert!(
            on.context_metrics
                .iter()
                .all(|m| m["job_id"].is_null() && m["snippets"].is_null())
        );
    }
}
