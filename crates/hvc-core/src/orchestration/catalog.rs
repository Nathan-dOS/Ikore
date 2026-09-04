//! The persisted six-stage vocabulary and its one historical alias.
use crate::{llm::AgentRole, persistence_service::PulsationStage};

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
