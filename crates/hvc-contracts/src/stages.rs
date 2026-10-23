//! The persisted six-stage vocabulary and its one historical alias.

use serde::{Deserialize, Serialize};

use crate::llm::AgentRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PulsationStage {
    #[serde(rename = "Intake")]
    Intake,
    #[serde(rename = "SPECIFICATION")]
    Specification,
    #[serde(rename = "Contract")]
    Contract,
    #[serde(rename = "Code")]
    Code,
    #[serde(rename = "Test")]
    Test,
    #[serde(rename = "Integrator")]
    Integrator,
}

impl PulsationStage {
    pub fn as_db_value(self) -> &'static str {
        STAGES
            .iter()
            .find(|(stage, _, _)| *stage == self)
            .expect("six stages")
            .1
    }

    pub fn from_db_value(raw: &str) -> Option<Self> {
        parse_stage(raw)
    }

    pub fn next(self) -> Option<Self> {
        match self {
            Self::Intake => Some(Self::Specification),
            Self::Specification => Some(Self::Contract),
            Self::Contract => Some(Self::Code),
            Self::Code => Some(Self::Test),
            Self::Test => Some(Self::Integrator),
            Self::Integrator => None,
        }
    }

    pub fn requires_checkpoint(self) -> bool {
        matches!(
            self,
            Self::Specification | Self::Contract | Self::Test | Self::Integrator
        )
    }
}

pub const STAGES: [(PulsationStage, &str, AgentRole); 6] = [
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

pub fn parse_stage(raw: &str) -> Option<PulsationStage> {
    let canonical = canonical_stage(raw);
    STAGES
        .iter()
        .find(|(_, name, _)| *name == canonical)
        .map(|(stage, _, _)| *stage)
}

pub fn canonical_stage(raw: &str) -> &str {
    if raw == "Specification" {
        "SPECIFICATION"
    } else {
        raw
    }
}

pub fn stage_role(raw: &str) -> Option<AgentRole> {
    let canonical = canonical_stage(raw);
    STAGES
        .iter()
        .find(|(_, name, _)| *name == canonical)
        .map(|(_, _, role)| *role)
}
