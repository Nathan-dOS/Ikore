use hvc_test_support::brownfield_benchmark::{VaultMode, run_sample};

#[test]
fn paired_rs1_fixture_measures_exact_resolution_and_equivalent_task_quality() {
    let empty = run_sample(VaultMode::EmptyVault, 1, false);
    let mapped = run_sample(VaultMode::PartialCoverage, 1, false);

    assert_eq!(empty.initial_fixture_sha256, mapped.initial_fixture_sha256);
    assert_eq!(empty.task_repository_sha256, mapped.task_repository_sha256);
    assert_eq!(
        empty.task.specification_quality_score,
        mapped.task.specification_quality_score
    );
    assert_eq!(
        empty.task.clarification_questions,
        mapped.task.clarification_questions
    );
    let mapping = mapped.mapping.expect("partial-coverage mapping evidence");
    assert_eq!(mapping.warm.cache_misses, 0);
    assert_eq!(mapping.exact_invalidation.cache_misses, 1);
    assert!(mapping.warm_resolution_reused);
    assert_eq!(mapping.resolution.precision_basis_points, 10_000);
    assert_eq!(mapping.resolution.recall_basis_points, 10_000);
    assert_eq!(mapping.resolution.false_resolved, 0);
    assert!(mapping.resolution.classification_mismatches.is_empty());
    assert!(mapping.extracted_partial_sources > 0);
}
