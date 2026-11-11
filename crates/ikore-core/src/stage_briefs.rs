//! Per-stage role contracts injected into the system prompt.
//!
//! The pipeline is designed so that the host does the heavy lifting and cheap,
//! compact models can execute each stage reliably. That only holds if the prompt
//! states the agent's role, its prohibitions and its tool contract explicitly:
//! a frontier model infers them, a small model does not.
//!
//! Every rule declared here has a matching host-side validation. Nothing is
//! promised to the model that the host does not already enforce.
use crate::persistence_service::PulsationStage;

/// Read-only tool contract. Identical for every stage and every backend.
///
/// The adapters already impose read-only execution (`OPENCODE_PERMISSION`,
/// `--sandbox read-only`, `--permission-mode dontAsk`), but until now the prompt
/// never said so: the model attempted `bash`, was denied without explanation,
/// retried, and stalled until the watchdog killed the run. Publication always
/// goes through the host from the JSON `artifacts` array, so files written
/// directly to disk never enter the pipeline on any backend.
pub const TOOL_CONTRACT: &str = "TOOLS AND FILE OUTPUT: You are running read-only. Writing files, running shell/bash commands, creating directories and executing tests are disabled by the host and will be rejected. Do not attempt them, and never retry a tool call that was rejected - retrying will stall the stage until it is killed. Anything written directly to disk is ignored: the pipeline publishes only what you return in the JSON `artifacts` array, after human approval. Read tools (read, glob, grep, list) are available for inspection, but the context you need is already supplied below.";

const INTAKE: &str = "ROLE: Triage only. Classify the request (feature, bug, refactor, spike, test or documentation), estimate its scope and risk level, identify whether the project is greenfield or legacy, and record the initial architectural decisions.
DO NOT: write or modify application code; design APIs, schemas or DTOs; produce source files (.rs, .ts, .js, .html and the like); plan implementation steps. Those belong to later stages.
DELIVER: classification notes and, at most, decision records, as `artifacts` entries of kind `stage_note` or `adr`.";

const SPECIFICATION: &str = "ROLE: Turn the approved intent into ONE actionable specification. Read the supplied context, map the files, modules and dependencies involved, and write the specification: objective, functional scope, files likely touched, explicit requirements, open questions, perceived risks, acceptance criteria and testing notes. If the supplied context already lists existing source files, treat the project as having a real baseline: reference every one that is relevant in `existingFileRefs`, and state in `decisions` why any of them is being superseded rather than reused. Never call a task greenfield when existing sources are present in your context - the host checks this.
DO NOT: implement code. You prepare instructions for the stages that follow.
DELIVER: at least one specification artifact whenever `statusUpdate` is `advance` or `checkpoint_ready`. `componentProposals` is a separate field and never substitutes for it; leaving `artifacts` empty while referencing artifact position 0 is always invalid.";

const CONTRACT: &str = "ROLE: Define the interfaces between layers: endpoints, request and response payloads, DTOs, schemas, events, and the authentication or authorization notes relevant to those contracts.
DO NOT: implement business logic, write handlers or services, or author tests.
DELIVER: a contract document as an artifact under `knowledge/obsidian/contracts/`, with every literal value it fixes (storage keys, field names, enum values) also listed verbatim under `contracted-literals` in its frontmatter, so later stages can be checked against it automatically. What you produce becomes immutable for the stages that follow.";

const CODE: &str = "ROLE: Implement business rules, services, handlers, repositories, endpoints and integrations exactly as the approved specification and contracts require. If a file already in your context conflicts with the approved contract, replace it as part of this unit's scope - never leave it in place alongside a new, incompatible implementation.
DO NOT: change an approved contract. If the implementation requires one to change, do not work around it - return `statusUpdate` `failed` and state which contract is wrong.
DELIVER: every created or modified file as its own `artifacts` entry, with `relativePath` set to the workspace-relative destination and `content` holding the complete final file, never a diff or a fragment. The host writes these files.";

const TEST: &str = "ROLE: Plan and author the tests that prove each acceptance criterion, then report coverage and results traceable to criterion IDs. Before assuming a framework or its globals (Jest, `describe`/`expect`, a browser's `window`/`document`), confirm the project's context already supplies that dependency and configuration; if it does not, author tests that run standalone under the runtime you know is available, and never gate the tests' own execution on an environment check that can silently be false - fail loudly instead of exiting clean having run nothing.
DO NOT: fix production code. Report failures and route them back; correcting them is another stage's job.
DELIVER: test files as artifacts with `relativePath`, plus a test report artifact. Never state that a test passed, failed or executed unless the supplied context contains its actual execution output; without that evidence, describe the tests as authored, not as run.";

const INTEGRATOR: &str = "ROLE: Verify consistency across code, tests, documentation and contracts. Check the code against the approved contracts, detect divergences, confirm the feature notes are synchronized, and produce the integration report with a per-check status. Host pre-checks that diff a contract's literal values against the delivered code, or flag a file no entrypoint reaches, take precedence over your own reading: report every one of them as a finding, never reconcile it away in prose.
DO NOT: implement new features or rewrite code to make a check pass. Report the divergence instead.
DELIVER: an integration report artifact. Documentation corrections may also be returned as artifacts.";

/// Role contract for a stage, accepting the historical `Specification` alias.
///
/// The match is exhaustive over [`PulsationStage`] on purpose: a seventh stage
/// must fail to compile rather than silently fall back to generic text.
pub fn role_brief(stage_name: &str) -> &'static str {
    match crate::orchestration::catalog::parse_stage(stage_name) {
        Some(PulsationStage::Intake) => INTAKE,
        Some(PulsationStage::Specification) => SPECIFICATION,
        Some(PulsationStage::Contract) => CONTRACT,
        Some(PulsationStage::Code) => CODE,
        Some(PulsationStage::Test) => TEST,
        Some(PulsationStage::Integrator) => INTEGRATOR,
        None => SPECIFICATION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::catalog::STAGES;
    use std::collections::HashSet;

    #[test]
    fn every_stage_has_a_distinct_non_empty_brief() {
        let mut seen = HashSet::new();
        for (_, name, _) in STAGES {
            let brief = role_brief(name);
            assert!(!brief.trim().is_empty(), "{name} has an empty brief");
            assert!(seen.insert(brief), "{name} reuses another stage's brief");
        }
        assert_eq!(seen.len(), STAGES.len());
    }

    #[test]
    fn every_brief_states_role_prohibitions_and_deliverable() {
        for (_, name, _) in STAGES {
            let brief = role_brief(name);
            for marker in ["ROLE:", "DO NOT:", "DELIVER:"] {
                assert!(brief.contains(marker), "{name} brief is missing {marker}");
            }
        }
    }

    #[test]
    fn specification_alias_resolves_to_the_same_brief() {
        assert_eq!(role_brief("Specification"), role_brief("SPECIFICATION"));
    }

    #[test]
    fn tool_contract_forbids_writes_and_forbids_retrying_a_denial() {
        assert!(TOOL_CONTRACT.contains("read-only"));
        assert!(TOOL_CONTRACT.contains("bash"));
        assert!(TOOL_CONTRACT.contains("never retry a tool call that was rejected"));
        assert!(TOOL_CONTRACT.contains("`artifacts`"));
    }

    #[test]
    fn intake_forbids_application_code_and_code_requires_full_files() {
        assert!(role_brief("Intake").contains("Triage only"));
        assert!(role_brief("Intake").contains("write or modify application code"));
        assert!(role_brief("Code").contains("never a diff or a fragment"));
        assert!(role_brief("Test").contains("describe the tests as authored, not as run"));
    }

    #[test]
    fn specification_requires_acknowledging_existing_context() {
        assert!(role_brief("SPECIFICATION").contains("existingFileRefs"));
        assert!(role_brief("SPECIFICATION").contains("Never call a task greenfield"));
    }

    #[test]
    fn contract_requires_a_contracted_literals_block() {
        assert!(role_brief("Contract").contains("contracted-literals"));
    }

    #[test]
    fn code_requires_replacing_conflicting_existing_files() {
        assert!(role_brief("Code").contains("replace it as part of this unit's scope"));
    }

    #[test]
    fn test_brief_forbids_silent_environment_gated_no_ops() {
        assert!(
            role_brief("Test").contains("fail loudly instead of exiting clean having run nothing")
        );
    }

    #[test]
    fn integrator_gives_precedence_to_host_literal_and_reachability_findings() {
        assert!(role_brief("Integrator").contains("take precedence over your own reading"));
    }
}
