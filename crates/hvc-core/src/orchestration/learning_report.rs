//! Consultative output: only host-supplied evidence IDs are admissible.
use super::{ContentSha256, ContractError};
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
            "---\ntype: learning_report\nstatus: proposed\nschema_version: 1\npulsation_id: {}\nintegration_epoch: {}\nreport_version: {}\ninput_sha256: {}\n---\n\n# Aprendizado consultivo\n\nProposta; não altera regras, código, fila ou a conclusão da pulsação.\n",
            serde_json::to_string(&input.pulsation_id).unwrap_or_default(),
            input.integration_epoch,
            input.report_version,
            ContentSha256::of(&source).as_str()
        );
        for (title, findings) in [
            ("Observações", &self.observations),
            ("Recomendações", &self.recommendations),
        ] {
            text.push_str(&format!("\n## {title}\n\n"));
            if findings.is_empty() {
                text.push_str("Nenhum achado sustentado.\n");
            }
            for finding in findings {
                text.push_str(&format!(
                    "- {}\n  Evidências: {}. Recorrente: {}.\n",
                    finding.text,
                    finding.evidence_ids.join(", "),
                    finding.recurring
                ));
            }
        }
        text.push_str("\n## Limitações\n\n");
        for limitation in &self.limitations {
            text.push_str(&format!("- {limitation}\n"));
        }
        text.push_str(&format!("- {} itens/trechos omitidos do input por orçamento; escopo local, não revisão global.\n",input.omitted));
        text.push_str("\n## Proveniência das evidências\n\n");
        for item in &input.evidence {
            text.push_str(&format!(
                "- `{}`: pulsação `{}`, {}.\n",
                item.id, item.pulsation_id, item.kind
            ));
        }
        Ok(text)
    }
}

pub(crate) fn schema() -> serde_json::Value {
    use serde_json::json;
    let finding = json!({"type":"object","additionalProperties":false,"required":["text","evidence_ids","recurring"],"properties":{"text":{"type":"string","minLength":1,"maxLength":1000},"evidence_ids":{"type":"array","minItems":1,"maxItems":8,"uniqueItems":true,"items":{"type":"string"}},"recurring":{"type":"boolean"}}});
    json!({"type":"object","additionalProperties":false,"required":["schema_version","observations","recommendations","limitations"],"properties":{"schema_version":{"type":"integer","const":1},"observations":{"type":"array","maxItems":8,"items":finding},"recommendations":{"type":"array","maxItems":6,"items":finding},"limitations":{"type":"array","maxItems":8,"items":{"type":"string","maxLength":1000}}}})
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
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearningActionRequest {
    pub job_id: String,
    pub expected_generation: u64,
}
