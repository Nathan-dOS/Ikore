//! Per-project composition settings (ADR-022). Composition is opt-in and off
//! by default; edits affect future admissions only, never a running stage.

use serde::{Deserialize, Serialize};

use super::{CompositionError, ErrorCode, Invariant, Sha256Hex, Validate, canonical_sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompositionSettings {
    /// New Forge-batch pulsations of the project are created as pipeline v4.
    pub enabled: bool,
    /// Bounded parallel scheduling cap. `1` is sequential; `2` requires the
    /// parity gate recorded in the qualification report before a project may
    /// select it, and the coordinator refuses anything above the contract.
    pub max_in_flight_units: u32,
    /// Trusted-project command execution for compile/test checks (M4).
    pub validation_enabled: bool,
    /// Recipes the developer promoted after reading the qualification
    /// report (M7): `code` and/or `test`. A recipe not listed here keeps one
    /// unit per stage even with composition enabled - the benchmark's arm B.
    #[serde(default)]
    pub qualified_recipes: Vec<String>,
    /// ADR-027 M5: the Visualizer writes notes on each artifact checkpoint's
    /// Review Brief. Off by default; the brief is complete without it.
    #[serde(default)]
    pub visualizer_enabled: bool,
    /// ADR-032 M4: human revision rounds of a pulsation (per input revision,
    /// since the last replan) after which every further Request revision
    /// needs an explicit decision. The default 3 is a hypothesis to evaluate.
    #[serde(default = "default_revision_round_threshold")]
    pub revision_round_threshold: u32,
}

pub const DEFAULT_REVISION_ROUND_THRESHOLD: u32 = 3;
pub const REVISION_ROUND_THRESHOLD_RANGE: std::ops::RangeInclusive<u32> = 1..=10;

const fn default_revision_round_threshold() -> u32 {
    DEFAULT_REVISION_ROUND_THRESHOLD
}

impl Default for CompositionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_in_flight_units: 1,
            validation_enabled: false,
            qualified_recipes: Vec::new(),
            visualizer_enabled: false,
            revision_round_threshold: DEFAULT_REVISION_ROUND_THRESHOLD,
        }
    }
}

impl Validate for CompositionSettings {
    fn validate(&self) -> Result<(), CompositionError> {
        if !(1..=2).contains(&self.max_in_flight_units) {
            return Err(CompositionError::new(
                Invariant::Sc006,
                ErrorCode::HostContractError,
                "maxInFlightUnits must be 1 or 2.",
            ));
        }
        if !REVISION_ROUND_THRESHOLD_RANGE.contains(&self.revision_round_threshold) {
            return Err(CompositionError::new(
                Invariant::Sc006,
                ErrorCode::HostContractError,
                "revisionRoundThreshold must be between 1 and 10.",
            ));
        }
        for recipe in &self.qualified_recipes {
            if !QUALIFIABLE_RECIPES.contains(&recipe.as_str()) {
                return Err(CompositionError::new(
                    Invariant::Sc006,
                    ErrorCode::HostContractError,
                    format!("`{recipe}` is not a recipe that plans units (code, test)."),
                ));
            }
        }
        Ok(())
    }
}

/// The recipes that plan several units and therefore need qualification.
pub const QUALIFIABLE_RECIPES: [&str; 2] = ["code", "test"];

impl CompositionSettings {
    pub fn sha256(&self) -> Result<Sha256Hex, CompositionError> {
        canonical_sha256(self, &[])
    }

    /// Whether a stage may plan several units from the approved proposals.
    pub fn recipe_qualified(&self, stage: super::Stage) -> bool {
        self.qualified_recipes
            .iter()
            .any(|recipe| recipe == stage.as_str())
    }

    /// Both planning recipes promoted - the configuration the integration
    /// suites and benchmark arms C/D run under.
    pub fn with_all_recipes_qualified(mut self) -> Self {
        self.qualified_recipes = QUALIFIABLE_RECIPES
            .iter()
            .map(|r| (*r).to_owned())
            .collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_is_off_by_default_and_bounded() {
        let settings = CompositionSettings::default();
        assert!(!settings.enabled);
        assert_eq!(settings.max_in_flight_units, 1);
        assert!(!settings.validation_enabled);
        settings.validate().unwrap();
        let parsed: CompositionSettings = serde_json::from_value(serde_json::json!({
            "enabled": true, "maxInFlightUnits": 2, "validationEnabled": false
        }))
        .unwrap();
        parsed.validate().unwrap();
        assert_ne!(parsed.sha256().unwrap(), settings.sha256().unwrap());
        let too_many = CompositionSettings {
            max_in_flight_units: 3,
            ..CompositionSettings::default()
        };
        assert!(too_many.validate().is_err());
        assert!(
            serde_json::from_value::<CompositionSettings>(serde_json::json!({"enabled": true}))
                .is_err()
        );
        // Qualification is opt-in per recipe and only for planning recipes.
        assert!(!settings.recipe_qualified(crate::orchestration::composition::Stage::Code));
        let promoted = CompositionSettings::default().with_all_recipes_qualified();
        assert!(promoted.recipe_qualified(crate::orchestration::composition::Stage::Test));
        assert!(!promoted.recipe_qualified(crate::orchestration::composition::Stage::Contract));
        let bogus = CompositionSettings {
            qualified_recipes: vec!["intake".into()],
            ..CompositionSettings::default()
        };
        assert!(bogus.validate().is_err());
        // Older rows without the field still parse.
        let legacy: CompositionSettings = serde_json::from_value(serde_json::json!({
            "enabled": true, "maxInFlightUnits": 1, "validationEnabled": true
        }))
        .unwrap();
        assert!(legacy.qualified_recipes.is_empty());
        // ADR-032: rows written before the threshold existed get the default,
        // and the threshold stays within its range.
        assert_eq!(
            legacy.revision_round_threshold,
            DEFAULT_REVISION_ROUND_THRESHOLD
        );
        for (threshold, valid) in [(0, false), (1, true), (10, true), (11, false)] {
            let settings = CompositionSettings {
                revision_round_threshold: threshold,
                ..CompositionSettings::default()
            };
            assert_eq!(settings.validate().is_ok(), valid, "{threshold}");
        }
        let round_trip: CompositionSettings = serde_json::from_value(
            serde_json::to_value(CompositionSettings {
                revision_round_threshold: 5,
                ..CompositionSettings::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(round_trip.revision_round_threshold, 5);
    }
}
