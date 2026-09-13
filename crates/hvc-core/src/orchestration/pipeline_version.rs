//! The single source of truth for pulsation pipeline versions.
//!
//! Every capability predicate that depends on `pulsations.pipeline_version` must
//! go through this module instead of repeating literals in SQL or Rust. The
//! database CHECK constraint is widened by migration; this enum documents what
//! each admitted value means and which subsystems may act on it.
//!
//! - V1: the original pulsation loop, pre-serial-integrity.
//! - V2: free-form pulsations with the coordinated, fenced serial pipeline.
//! - V3: pulsations created from a frozen Feature Forge task batch.
//! - V4: a V3 batch executed through host-coordinated stage composition
//!   (ADR-022). Admitted by the schema only after migration V32; until then
//!   no row can carry it, so referencing it here changes no behaviour.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum PipelineVersion {
    V1,
    V2,
    V3,
    V4,
}

impl PipelineVersion {
    /// Pulsations created without a task batch (`create_pulsation`).
    pub const FREE_FORM: Self = Self::V2;
    /// Pulsations created from a frozen Forge batch, executed as one model
    /// call per stage.
    pub const BATCH: Self = Self::V3;
    /// Pulsations created from a frozen Forge batch, executed through
    /// host-coordinated stage composition.
    pub const COMPOSED_BATCH: Self = Self::V4;

    pub const ALL: [Self; 4] = [Self::V1, Self::V2, Self::V3, Self::V4];

    pub const fn as_u8(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
            Self::V3 => 3,
            Self::V4 => 4,
        }
    }

    pub const fn from_db(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            3 => Some(Self::V3),
            4 => Some(Self::V4),
            _ => None,
        }
    }

    /// The version a newly created batch pulsation receives. Composition is a
    /// per-project opt-in; the caller resolves that setting before creating
    /// the pulsation, and the decision is frozen on the row.
    pub const fn for_new_batch(composition_enabled: bool) -> Self {
        if composition_enabled {
            Self::COMPOSED_BATCH
        } else {
            Self::BATCH
        }
    }

    /// Whether the stage job runs through `orchestration::composition` instead
    /// of the single-call runtime.
    pub const fn supports_composition(self) -> bool {
        matches!(self, Self::V4)
    }

    /// Whether the pulsation carries a frozen Forge batch manifest.
    pub const fn uses_batch(self) -> bool {
        matches!(self, Self::V3 | Self::V4)
    }

    /// Whether consultative Learning may run after the pulsation ends.
    pub const fn learning_eligible(self) -> bool {
        matches!(self, Self::V2 | Self::V3 | Self::V4)
    }

    /// SQL list literal of Learning-eligible versions, for embedding in a
    /// `pipeline_version IN (...)` predicate. Generated from the enum so the
    /// SQL cannot drift from `learning_eligible`.
    pub fn learning_eligible_sql_list() -> String {
        let versions = Self::ALL
            .iter()
            .filter(|version| version.learning_eligible())
            .map(|version| version.as_u8().to_string())
            .collect::<Vec<_>>();
        format!("({})", versions.join(","))
    }
}

impl TryFrom<u8> for PipelineVersion {
    type Error = super::ContractError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_db(value).ok_or(super::ContractError::UnsupportedVersion(value))
    }
}

impl From<PipelineVersion> for u8 {
    fn from(value: PipelineVersion) -> Self {
        value.as_u8()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_round_trip_and_reject_unknown_values() {
        for version in PipelineVersion::ALL {
            assert_eq!(PipelineVersion::from_db(version.as_u8()), Some(version));
            let json = serde_json::to_value(version).unwrap();
            assert_eq!(json, serde_json::json!(version.as_u8()));
            assert_eq!(
                serde_json::from_value::<PipelineVersion>(json).unwrap(),
                version
            );
        }
        assert_eq!(PipelineVersion::from_db(0), None);
        assert_eq!(PipelineVersion::from_db(5), None);
        assert!(serde_json::from_value::<PipelineVersion>(serde_json::json!(5)).is_err());
    }

    #[test]
    fn only_v4_composes_and_only_batches_carry_a_manifest() {
        assert!(!PipelineVersion::V1.supports_composition());
        assert!(!PipelineVersion::V2.supports_composition());
        assert!(!PipelineVersion::V3.supports_composition());
        assert!(PipelineVersion::V4.supports_composition());
        assert!(!PipelineVersion::V1.uses_batch());
        assert!(!PipelineVersion::V2.uses_batch());
        assert!(PipelineVersion::V3.uses_batch());
        assert!(PipelineVersion::V4.uses_batch());
        assert_eq!(PipelineVersion::for_new_batch(false), PipelineVersion::V3);
        assert_eq!(PipelineVersion::for_new_batch(true), PipelineVersion::V4);
    }

    #[test]
    fn learning_eligibility_matches_the_generated_sql_list() {
        assert!(!PipelineVersion::V1.learning_eligible());
        assert_eq!(PipelineVersion::learning_eligible_sql_list(), "(2,3,4)");
    }
}
