//! The M7 qualification harness itself: the fixture catalog is well formed
//! and one fixture runs through a legacy arm and a composed arm in fixture
//! mode with the oracle passing. The full matrix is the example
//! `composition_benchmark`, whose reports live under `quality/`.
use ikore_test_support::composition_benchmark::*;

#[test]
fn the_fixture_catalog_covers_four_families_with_a_held_out_set() {
    let fixtures = load_fixtures();
    assert_eq!(fixtures.len(), 20);
    for family in [
        Family::SimpleMaintenance,
        Family::CohesiveMultiFile,
        Family::SeparableMultiTask,
        Family::FailureRecovery,
    ] {
        let of_family: Vec<_> = fixtures.iter().filter(|f| f.family == family).collect();
        assert_eq!(of_family.len(), 5, "{family:?}");
        assert_eq!(
            of_family.iter().filter(|f| f.held_out).count(),
            1,
            "{family:?}"
        );
    }
    for fixture in &fixtures {
        assert!(!fixture.oracle.is_empty(), "{}", fixture.id);
        assert!(!fixture.units.is_empty(), "{}", fixture.id);
        for unit in &fixture.units {
            assert!(
                fixture.outputs.contains_key(&unit.destination_path),
                "{}: {}",
                fixture.id,
                unit.unit_key
            );
            assert!(unit.task < fixture.tasks.len());
        }
        assert_eq!(
            fixture.fault.is_some(),
            fixture.family == Family::FailureRecovery,
            "{}",
            fixture.id
        );
    }
    assert_eq!(fixtures_sha256(&fixtures).len(), 64);
}

#[test]
fn a_fixture_runs_through_a_legacy_and_a_composed_arm_and_passes_its_oracle() {
    let fixtures = load_fixtures();
    let fixture = fixtures
        .iter()
        .find(|f| f.id == "recovery-01-transport-on-code")
        .unwrap();
    for arm in [Arm::A, Arm::D] {
        let observation = run_observation(arm, fixture, &Mode::Fixture);
        assert!(
            observation.completed,
            "{arm:?}: {:?}",
            observation.diagnostics
        );
        assert!(
            observation.correct,
            "{arm:?}: {:?}",
            observation.oracle_failures
        );
        assert_eq!(
            observation.provider_calls, 7,
            "{arm:?}: six stages plus one transport retry"
        );
        assert_eq!(observation.interventions, 0, "{arm:?}");
        assert!(observation.input_tokens > 0 && observation.output_tokens > 0);
        assert!(
            observation.estimated_cost_micro_usd.is_none(),
            "fixture mode reports no cost"
        );
    }
}
