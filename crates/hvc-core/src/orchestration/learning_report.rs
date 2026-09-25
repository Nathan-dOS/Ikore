//! Consultative output: only host-supplied evidence IDs are admissible.
use super::{ContentSha256, ContractError};
use crate::agent_output_language::validate_english_prose;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningEvidence {
    pub id: String,
    pub pulsation_id: String,
    pub kind: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningInput {
    pub pulsation_id: String,
    pub integration_epoch: u64,
    pub report_version: u32,
    pub evidence: Vec<LearningEvidence>,
    pub omitted: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningFinding {
    pub text: String,
    pub evidence_ids: Vec<String>,
    pub recurring: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningReportV1 {
    pub schema_version: u8,
    pub observations: Vec<LearningFinding>,
    pub recommendations: Vec<LearningFinding>,
    pub limitations: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningRefinementMode {
    #[default]
    Observe,
    Propose,
}

impl LearningRefinementMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Propose => "propose",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "observe" => Some(Self::Observe),
            "propose" => Some(Self::Propose),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningContextRunInput {
    pub retrieval_run_id: String,
    pub stage_name: String,
    pub considered_total: usize,
    pub delivered_total: usize,
    pub cited_total: usize,
    pub fallback_used: bool,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningGraphCandidate {
    pub candidate_id: String,
    pub source_path: String,
    pub target_path: String,
    pub relation: String,
    pub source_expected_sha256: String,
    pub target_expected_sha256: String,
    pub preview_diff: String,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningInputV2 {
    pub pulsation_id: String,
    pub integration_epoch: u64,
    pub report_version: u32,
    pub refinement_mode: LearningRefinementMode,
    pub evidence: Vec<LearningEvidence>,
    pub context_runs: Vec<LearningContextRunInput>,
    pub graph_candidates: Vec<LearningGraphCandidate>,
    pub omitted: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAssessmentVerdict {
    Material,
    Supporting,
    NoObservableUse,
    Indeterminate,
}

impl ContextAssessmentVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Material => "material",
            Self::Supporting => "supporting",
            Self::NoObservableUse => "no_observable_use",
            Self::Indeterminate => "indeterminate",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningContextAssessment {
    pub retrieval_run_id: String,
    pub verdict: ContextAssessmentVerdict,
    pub evidence_ids: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningGraphSelection {
    pub candidate_id: String,
    pub evidence_ids: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningReportV2 {
    pub schema_version: u8,
    pub observations: Vec<LearningFinding>,
    pub recommendations: Vec<LearningFinding>,
    pub context_assessments: Vec<LearningContextAssessment>,
    pub graph_selections: Vec<LearningGraphSelection>,
    pub limitations: Vec<String>,
}

impl LearningReportV2 {
    pub fn validate(&self, input: &LearningInputV2) -> Result<(), ContractError> {
        if self.schema_version != 2
            || self.observations.len() > 8
            || self.recommendations.len() > 6
            || self.context_assessments.len() > input.context_runs.len()
            || self.graph_selections.len() > 8
            || self.limitations.len() > 8
            || self
                .limitations
                .iter()
                .any(|value| value.chars().count() > 1000)
        {
            return Err(ContractError::Invalid("limites do relatório Learning V2"));
        }
        validate_findings(
            self.observations.iter().chain(&self.recommendations),
            &input.evidence,
        )?;
        let allowed_evidence = input
            .evidence
            .iter()
            .map(|item| item.id.as_str())
            .collect::<HashSet<_>>();
        let allowed_runs = input
            .context_runs
            .iter()
            .map(|run| run.retrieval_run_id.as_str())
            .collect::<HashSet<_>>();
        let mut assessed = HashSet::new();
        for assessment in &self.context_assessments {
            if !allowed_runs.contains(assessment.retrieval_run_id.as_str())
                || !assessed.insert(assessment.retrieval_run_id.as_str())
                || assessment.rationale.trim().is_empty()
                || assessment.rationale.chars().count() > 1000
                || assessment
                    .evidence_ids
                    .iter()
                    .any(|id| !allowed_evidence.contains(id.as_str()))
            {
                return Err(ContractError::Invalid("avaliação de contexto inválida"));
            }
        }
        let candidates = input
            .graph_candidates
            .iter()
            .map(|candidate| (candidate.candidate_id.as_str(), candidate))
            .collect::<HashMap<_, _>>();
        let mut selected = HashSet::new();
        for selection in &self.graph_selections {
            let Some(candidate) = candidates.get(selection.candidate_id.as_str()) else {
                return Err(ContractError::Invalid("candidate_id de grafo desconhecido"));
            };
            let operational = candidate.evidence_ids.iter().collect::<HashSet<_>>();
            if !selected.insert(selection.candidate_id.as_str())
                || selection.rationale.trim().is_empty()
                || selection.rationale.chars().count() > 1000
                || selection.evidence_ids.is_empty()
                || selection
                    .evidence_ids
                    .iter()
                    .any(|id| !allowed_evidence.contains(id.as_str()) || !operational.contains(id))
            {
                return Err(ContractError::Invalid(
                    "seleção de grafo sem evidência operacional conhecida",
                ));
            }
        }
        if input.refinement_mode == LearningRefinementMode::Observe
            && !self.graph_selections.is_empty()
        {
            return Err(ContractError::Invalid(
                "modo Observe não aceita propostas de grafo",
            ));
        }
        validate_learning_report_english(self, &developer_titles(input))?;
        Ok(())
    }

    pub fn markdown(&self, input: &LearningInputV2) -> Result<String, ContractError> {
        self.validate(input)?;
        let source =
            serde_json::to_vec(input).map_err(|_| ContractError::Invalid("input Learning V2"))?;
        let mut text = format!(
            "---\ntype: learning_report\nstatus: proposed\nlanguage: en\nschema_version: 2\npulsation_id: {}\nintegration_epoch: {}\nreport_version: {}\ninput_sha256: {}\n---\n\n# Consultative learning report\n\nThis report is non-normative. Graph proposals require host publication after human approval.\n",
            serde_json::to_string(&input.pulsation_id).unwrap_or_default(),
            input.integration_epoch,
            input.report_version,
            ContentSha256::of(&source).as_str()
        );
        for (title, findings) in [
            ("Observations", &self.observations),
            ("Recommendations", &self.recommendations),
        ] {
            text.push_str(&format!("\n## {title}\n\n"));
            if findings.is_empty() {
                text.push_str("No supported finding.\n");
            }
            for finding in findings {
                text.push_str(&format!(
                    "- {}\n  Evidence: {}. Recurring: {}.\n",
                    finding.text,
                    finding.evidence_ids.join(", "),
                    finding.recurring
                ));
            }
        }
        text.push_str("\n## Context audit\n\n");
        if self.context_assessments.is_empty() {
            text.push_str(
                "No observable conclusion; the audit is indeterminate or lacks evidence.\n",
            );
        }
        for assessment in &self.context_assessments {
            text.push_str(&format!(
                "- `{}` — `{}`: {} Evidence: {}.\n",
                assessment.retrieval_run_id,
                assessment.verdict.as_str(),
                assessment.rationale,
                assessment.evidence_ids.join(", ")
            ));
        }
        text.push_str("\n## Selected graph candidates\n\n");
        if self.graph_selections.is_empty() {
            text.push_str("No change was selected.\n");
        }
        for selection in &self.graph_selections {
            text.push_str(&format!(
                "- `{}`: {} Evidence: {}.\n",
                selection.candidate_id,
                selection.rationale,
                selection.evidence_ids.join(", ")
            ));
        }
        text.push_str("\n## Limitations\n\n");
        for limitation in &self.limitations {
            text.push_str(&format!("- {limitation}\n"));
        }
        text.push_str(&format!(
            "- {} items or excerpts were omitted by the budget; scope is local.\n",
            input.omitted
        ));
        Ok(text)
    }
}

fn validate_findings<'a>(
    findings: impl Iterator<Item = &'a LearningFinding>,
    evidence: &[LearningEvidence],
) -> Result<(), ContractError> {
    let allowed = evidence
        .iter()
        .map(|item| (item.id.as_str(), item.pulsation_id.as_str()))
        .collect::<HashMap<_, _>>();
    for finding in findings {
        if finding.text.trim().is_empty()
            || finding.text.chars().count() > 1000
            || finding.evidence_ids.is_empty()
            || finding.evidence_ids.len() > 8
        {
            return Err(ContractError::Invalid(
                "achado Learning sem evidência ou acima do orçamento",
            ));
        }
        let mut pulses = HashSet::new();
        let mut ids = HashSet::new();
        for id in &finding.evidence_ids {
            let Some(pulse) = allowed.get(id.as_str()) else {
                return Err(ContractError::Invalid("evidência Learning desconhecida"));
            };
            if !ids.insert(id) {
                return Err(ContractError::Invalid("evidência Learning duplicada"));
            }
            pulses.insert(pulse);
        }
        if finding.recurring && pulses.len() < 2 {
            return Err(ContractError::Invalid(
                "recorrência exige pelo menos duas pulsações",
            ));
        }
    }
    Ok(())
}

/// Pulsation titles the developer wrote reach the report as host evidence
/// (the current request and `historical_summary` lines such as
/// `failed: teste 3`). Quoting them verbatim is correct, but the English guard
/// judged a title like `teste 3` as Portuguese prose and cost a whole extra
/// Learning call (ToDoListWebView, 2026-09-22). Only these host-supplied
/// names, short enough to be a title rather than a request body, are exempt.
fn developer_titles(input: &LearningInputV2) -> Vec<&str> {
    let mut titles = input
        .evidence
        .iter()
        .filter_map(|evidence| match evidence.kind.as_str() {
            "historical_summary" => evidence.text.split_once(": ").map(|(_, title)| title),
            "completed_request" => Some(evidence.text.as_str()),
            _ => None,
        })
        .map(str::trim)
        .filter(|title| !title.is_empty() && title.chars().count() <= 80)
        .collect::<Vec<_>>();
    // Longest first, so a title never leaves a fragment of a longer one.
    titles.sort_by_key(|title| std::cmp::Reverse(title.len()));
    titles.dedup();
    titles
}

fn validate_learning_report_english(
    report: &LearningReportV2,
    titles: &[&str],
) -> Result<(), ContractError> {
    let text_fields = report
        .observations
        .iter()
        .chain(&report.recommendations)
        .map(|finding| finding.text.as_str())
        .chain(
            report
                .context_assessments
                .iter()
                .map(|assessment| assessment.rationale.as_str()),
        )
        .chain(
            report
                .graph_selections
                .iter()
                .map(|selection| selection.rationale.as_str()),
        )
        .chain(report.limitations.iter().map(String::as_str));
    for text in text_fields {
        let prose = titles
            .iter()
            .fold(text.to_owned(), |prose, title| prose.replace(title, " "));
        validate_english_prose("Learning report text", &prose)
            .map_err(|_| ContractError::Invalid("Learning report must be English"))?;
    }
    Ok(())
}

impl LearningReportV1 {
    pub fn validate(&self, input: &LearningInput) -> Result<(), ContractError> {
        if self.schema_version != 1
            || self.observations.len() > 8
            || self.recommendations.len() > 6
            || self.limitations.len() > 8
            || self.limitations.iter().any(|s| s.chars().count() > 1000)
        {
            return Err(ContractError::Invalid("limites do relatório Learning"));
        }
        let allowed: HashMap<_, _> = input
            .evidence
            .iter()
            .map(|e| (e.id.as_str(), e.pulsation_id.as_str()))
            .collect();
        for finding in self.observations.iter().chain(&self.recommendations) {
            if finding.text.trim().is_empty()
                || finding.text.chars().count() > 1000
                || finding.evidence_ids.is_empty()
                || finding.evidence_ids.len() > 8
            {
                return Err(ContractError::Invalid(
                    "achado Learning sem evidência ou acima do orçamento",
                ));
            }
            let mut pulses = HashSet::new();
            let mut ids = HashSet::new();
            for id in &finding.evidence_ids {
                let Some(pulse) = allowed.get(id.as_str()) else {
                    return Err(ContractError::Invalid("evidência Learning desconhecida"));
                };
                if !ids.insert(id) {
                    return Err(ContractError::Invalid("evidência Learning duplicada"));
                }
                pulses.insert(pulse);
            }
            if finding.recurring && pulses.len() < 2 {
                return Err(ContractError::Invalid(
                    "recorrência exige pelo menos duas pulsações",
                ));
            }
        }
        Ok(())
    }
    pub fn markdown(&self, input: &LearningInput) -> Result<String, ContractError> {
        self.validate(input)?;
        let source =
            serde_json::to_vec(input).map_err(|_| ContractError::Invalid("input Learning"))?;
        let mut text = format!(
            "---\ntype: learning_report\nstatus: proposed\nlanguage: en\nschema_version: 1\npulsation_id: {}\nintegration_epoch: {}\nreport_version: {}\ninput_sha256: {}\n---\n\n# Consultative learning report\n\nThis proposal does not change rules, code, queues, or the pulsation outcome.\n",
            serde_json::to_string(&input.pulsation_id).unwrap_or_default(),
            input.integration_epoch,
            input.report_version,
            ContentSha256::of(&source).as_str()
        );
        for (title, findings) in [
            ("Observations", &self.observations),
            ("Recommendations", &self.recommendations),
        ] {
            text.push_str(&format!("\n## {title}\n\n"));
            if findings.is_empty() {
                text.push_str("No supported finding.\n");
            }
            for finding in findings {
                text.push_str(&format!(
                    "- {}\n  Evidence: {}. Recurring: {}.\n",
                    finding.text,
                    finding.evidence_ids.join(", "),
                    finding.recurring
                ));
            }
        }
        text.push_str("\n## Limitations\n\n");
        for limitation in &self.limitations {
            text.push_str(&format!("- {limitation}\n"));
        }
        text.push_str(&format!("- {} input items or excerpts were omitted by the budget; scope is local, not a global review.\n",input.omitted));
        text.push_str("\n## Evidence provenance\n\n");
        for item in &input.evidence {
            text.push_str(&format!(
                "- `{}`: pulsation `{}`, {}.\n",
                item.id, item.pulsation_id, item.kind
            ));
        }
        Ok(text)
    }
}

#[allow(dead_code)] // Retained to read and validate historical V1 reports without migration.
pub(crate) fn schema() -> serde_json::Value {
    use serde_json::json;
    let finding = json!({"type":"object","additionalProperties":false,"required":["text","evidence_ids","recurring"],"properties":{"text":{"type":"string","minLength":1,"maxLength":1000},"evidence_ids":{"type":"array","minItems":1,"maxItems":8,"uniqueItems":true,"items":{"type":"string"}},"recurring":{"type":"boolean"}}});
    json!({"type":"object","additionalProperties":false,"required":["schema_version","observations","recommendations","limitations"],"properties":{"schema_version":{"type":"integer","const":1},"observations":{"type":"array","maxItems":8,"items":finding},"recommendations":{"type":"array","maxItems":6,"items":finding},"limitations":{"type":"array","maxItems":8,"items":{"type":"string","maxLength":1000}}}})
}

pub(crate) fn schema_v2() -> serde_json::Value {
    use serde_json::json;
    let finding = json!({"type":"object","additionalProperties":false,"required":["text","evidence_ids","recurring"],"properties":{"text":{"type":"string","minLength":1,"maxLength":1000},"evidence_ids":{"type":"array","minItems":1,"maxItems":8,"uniqueItems":true,"items":{"type":"string"}},"recurring":{"type":"boolean"}}});
    let assessment = json!({"type":"object","additionalProperties":false,"required":["retrieval_run_id","verdict","evidence_ids","rationale"],"properties":{"retrieval_run_id":{"type":"string","minLength":1},"verdict":{"type":"string","enum":["material","supporting","no_observable_use","indeterminate"]},"evidence_ids":{"type":"array","maxItems":8,"uniqueItems":true,"items":{"type":"string"}},"rationale":{"type":"string","minLength":1,"maxLength":1000}}});
    let selection = json!({"type":"object","additionalProperties":false,"required":["candidate_id","evidence_ids","rationale"],"properties":{"candidate_id":{"type":"string","minLength":1},"evidence_ids":{"type":"array","minItems":1,"maxItems":8,"uniqueItems":true,"items":{"type":"string"}},"rationale":{"type":"string","minLength":1,"maxLength":1000}}});
    json!({"type":"object","additionalProperties":false,"required":["schema_version","observations","recommendations","context_assessments","graph_selections","limitations"],"properties":{"schema_version":{"type":"integer","const":2},"observations":{"type":"array","maxItems":8,"items":finding},"recommendations":{"type":"array","maxItems":6,"items":finding},"context_assessments":{"type":"array","maxItems":6,"items":assessment},"graph_selections":{"type":"array","maxItems":8,"items":selection},"limitations":{"type":"array","maxItems":8,"items":{"type":"string","maxLength":1000}}}})
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextAuditStageSnapshot {
    pub stage_name: String,
    pub considered: u32,
    pub delivered: u32,
    pub cited: u32,
    pub audited: u32,
    pub indeterminate: u32,
    pub observed_use: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LearningGraphProposalSnapshot {
    pub id: String,
    pub candidate_id: String,
    pub source_path: String,
    pub target_path: String,
    pub relation: String,
    pub preview_diff: String,
    pub rationale: String,
    pub evidence_ids: Vec<String>,
    pub decision: String,
    pub publication_state: String,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LearningRunSnapshot {
    pub job_id: String,
    pub generation: u64,
    pub budget_calls: u32,
    pub total_calls: u32,
    pub budget_grants: u32,
    pub artifact_id: Option<String>,
    pub diagnostic: Option<String>,
    pub context_audit: Vec<ContextAuditStageSnapshot>,
    pub graph_proposals: Vec<LearningGraphProposalSnapshot>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearningActionRequest {
    pub job_id: String,
    pub expected_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningGraphDecision {
    Approve,
    Reject,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewLearningGraphDecision {
    pub proposal_id: String,
    pub decision: LearningGraphDecision,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewLearningGraphRequest {
    pub job_id: String,
    pub expected_generation: u64,
    pub decisions: Vec<ReviewLearningGraphDecision>,
}
