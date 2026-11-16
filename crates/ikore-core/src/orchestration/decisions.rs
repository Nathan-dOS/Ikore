//! Read-only projection of existing human decisions. These values never grant
//! authority: trusted UI actions still call the domain's guarded commands.
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::companion_dock::require_project;
use crate::orchestration::{ExecutionFence, SpecificationQuestionSet};
use crate::persistence_service::{PersistenceService, PersistenceServiceError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingDecisionKind {
    ArtifactReview,
    Clarification,
    PathConfirmation,
    StageFailure,
    ScopeRequest,
    CliFallback,
    ForgeQuestions,
    DescriptionReview,
    BaseReview,
    Delivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingDecisionAction {
    OpenFullReview,
    Approve,
    RequestRevision,
    Reject,
    AnswerQuestions,
    Retry,
    Abort,
    GrantScope,
    OpenAgents,
    OpenForge,
    OpenDelivery,
    OpenReconciliation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingForgeQuestions {
    pub feature_id: String,
    pub feature_revision: u64,
    pub turn_id: String,
    pub questions: Vec<String>,
    pub question_details: Vec<crate::feature_forge::FeatureQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDecision {
    pub id: String,
    pub project_id: String,
    pub kind: PendingDecisionKind,
    pub source_id: String,
    pub pulsation_id: Option<String>,
    pub checkpoint_id: Option<String>,
    pub feature_id: Option<String>,
    pub input_revision: Option<u64>,
    pub state_version: u64,
    pub stage: Option<String>,
    pub title: String,
    pub description: String,
    pub actions: Vec<PendingDecisionAction>,
    pub question_set: Option<SpecificationQuestionSet>,
    #[serde(default)]
    pub forge_questions: Option<PendingForgeQuestions>,
}

/// The newest discovery reply is the sole source of clickable questions.
/// Its input revision precedes the brief revision it committed by one.
pub(crate) fn current_feature_questions_on(
    db: &Connection,
    project: &str,
    feature: &str,
    revision: u64,
) -> Result<Option<PendingForgeQuestions>, PersistenceServiceError> {
    let row:Option<(String,Option<String>)>=db.query_row(
        "SELECT t.id,t.response_json FROM forge_turns t JOIN forge_features f ON f.id=t.feature_id WHERE f.id=?1 AND f.project_id=?2 AND f.revision=?3 AND t.kind='discovery' AND t.status='completed' AND t.feature_revision+1=f.revision AND t.id=(SELECT id FROM forge_turns WHERE feature_id=f.id AND kind='discovery' ORDER BY ordinal DESC LIMIT 1)",
        rusqlite::params![feature,project,revision],|row|Ok((row.get(0)?,row.get(1)?)),
    ).optional()?;
    let Some((turn_id, Some(response))) = row else {
        return Ok(None);
    };
    let response: serde_json::Value =
        serde_json::from_str(&response).map_err(|error| invalid(error.to_string()))?;
    let questions: Vec<String> = serde_json::from_value(
        response
            .get("questions")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| invalid(error.to_string()))?;
    let question_details: Vec<crate::feature_forge::FeatureQuestion> = serde_json::from_value(
        response
            .get("questionDetails")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| invalid(error.to_string()))?;
    if questions.is_empty() || questions.len() > 3 {
        return Ok(None);
    }
    Ok(Some(PendingForgeQuestions {
        feature_id: feature.into(),
        feature_revision: revision,
        turn_id,
        questions,
        question_details,
    }))
}

fn invalid(message: impl Into<String>) -> PersistenceServiceError {
    PersistenceServiceError::Runtime(message.into())
}

fn safe(text: &str, limit: usize) -> String {
    crate::secret_redactor::redact_secrets(text)
        .map(|result| result.text.chars().take(limit).collect())
        .unwrap_or_else(|_| "Details unavailable after sanitization.".into())
}

pub(crate) fn list_pending_decisions_on(
    db: &Connection,
    project: &str,
) -> Result<Vec<PendingDecision>, PersistenceServiceError> {
    require_project(db, project)?;
    let rows = db.prepare(
        "SELECT c.id,c.pulsation_id,c.purpose,p.feature_name,p.input_revision,p.state_version,c.stage,
          f.allow_retry,f.allow_revision,f.diagnostic,cf.diagnostic,
          q.id,q.generation,q.conversation_id,q.questions_json,c.producer_job_id,
          EXISTS(SELECT 1 FROM mandate_scope_requests s WHERE s.checkpoint_id=c.id AND s.state='pending' AND json_array_length(s.accepted_json)>0)
         FROM checkpoints c JOIN pulsations p ON p.id=c.pulsation_id
         LEFT JOIN stage_failure_requests f ON f.checkpoint_id=c.id AND f.decision IS NULL
         LEFT JOIN cli_fallback_requests cf ON cf.checkpoint_id=c.id AND cf.decision IS NULL
         LEFT JOIN specification_question_sets q ON q.checkpoint_id=c.id AND q.answers_json IS NULL AND q.input_revision=p.input_revision AND q.state_version=p.state_version
         WHERE p.project_id=?1 AND p.status='checkpoint' AND c.status='waiting'
           AND c.input_revision=p.input_revision AND c.state_version=p.state_version
           AND (c.stage=p.stage OR (c.stage='Specification' AND p.stage='SPECIFICATION'))
         ORDER BY c.created_at,c.id"
    )?.query_map([project], |row| Ok((
        row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?,row.get::<_, String>(3)?,
        row.get::<_, u64>(4)?,row.get::<_, u64>(5)?,row.get::<_, String>(6)?,
        row.get::<_, Option<bool>>(7)?,row.get::<_, Option<bool>>(8)?,row.get::<_, Option<String>>(9)?,row.get::<_, Option<String>>(10)?,
        row.get::<_, Option<String>>(11)?,row.get::<_, Option<u64>>(12)?,row.get::<_, Option<String>>(13)?,row.get::<_, Option<String>>(14)?,row.get::<_, Option<String>>(15)?,row.get::<_, bool>(16)?
    )))?.collect::<Result<Vec<_>,_>>()?;
    let mut decisions = Vec::new();
    for (
        checkpoint,
        pulse,
        purpose,
        name,
        revision,
        state,
        stage,
        retry,
        revise,
        failure,
        fallback,
        set_id,
        generation,
        conversation,
        questions,
        job,
        scope,
    ) in rows
    {
        let (kind, mut description, mut actions) = match purpose.as_str() {
            "artifact_review" => (
                PendingDecisionKind::ArtifactReview,
                format!(
                    "{stage} is ready for your review. Read the persisted documents before deciding."
                ),
                vec![
                    PendingDecisionAction::OpenFullReview,
                    PendingDecisionAction::Approve,
                    PendingDecisionAction::RequestRevision,
                    PendingDecisionAction::Reject,
                ],
            ),
            "clarification" if set_id.is_some() => (
                PendingDecisionKind::Clarification,
                "Specification needs your answers before it can continue.".into(),
                vec![PendingDecisionAction::AnswerQuestions],
            ),
            "path_confirmation" => (
                PendingDecisionKind::PathConfirmation,
                "Confirm the planned file destinations before Specification starts.".into(),
                vec![PendingDecisionAction::OpenFullReview],
            ),
            "stage_failure" if retry.is_some() => {
                let mut actions = vec![PendingDecisionAction::Abort];
                if retry == Some(true) {
                    actions.insert(0, PendingDecisionAction::Retry);
                }
                if revise == Some(true) {
                    actions.insert(0, PendingDecisionAction::RequestRevision);
                }
                (
                    if scope {
                        PendingDecisionKind::ScopeRequest
                    } else {
                        PendingDecisionKind::StageFailure
                    },
                    failure.unwrap_or_default(),
                    actions,
                )
            }
            "cli_fallback" if fallback.is_some() => (
                PendingDecisionKind::CliFallback,
                fallback.unwrap_or_default(),
                vec![PendingDecisionAction::OpenAgents],
            ),
            _ => continue,
        };
        if scope {
            actions.insert(0, PendingDecisionAction::GrantScope);
        }
        if kind == PendingDecisionKind::ArtifactReview {
            let checkpoint = crate::persistence_service::HitlCheckpoint {
                checkpoint_id: checkpoint.clone(),
                pulsation_id: pulse.clone(),
                state_version: state,
            };
            match PersistenceService::checkpoint_brief_summary_on(db, &checkpoint) {
                Ok(lines) if !lines.is_empty() => {
                    description.push_str("\n\n");
                    description.push_str(&lines.join("\n"));
                }
                Err(_) => {
                    description.push_str("\nStructured brief unavailable; open the full review.")
                }
                _ => {}
            }
        }
        let question_set = match (set_id, generation, conversation, questions, job) {
            (Some(id), Some(generation), Some(conversation_id), Some(json), Some(job_id)) => {
                Some(SpecificationQuestionSet {
                    id,
                    checkpoint_id: checkpoint.clone(),
                    conversation_id,
                    fence: ExecutionFence {
                        project_id: project.into(),
                        pulsation_id: Some(pulse.clone()),
                        input_revision: Some(revision),
                        job_id,
                        generation,
                        state_version: state,
                    },
                    questions: serde_json::from_str(&json)
                        .map_err(|error| invalid(error.to_string()))?,
                })
            }
            _ => None,
        };
        decisions.push(PendingDecision {
            id: format!("checkpoint:{checkpoint}"),
            project_id: project.into(),
            kind,
            source_id: checkpoint.clone(),
            pulsation_id: Some(pulse),
            checkpoint_id: Some(checkpoint),
            feature_id: None,
            input_revision: Some(revision),
            state_version: state,
            stage: Some(stage),
            title: safe(&name, 256),
            description: safe(&description, 4000),
            actions,
            question_set,
            forge_questions: None,
        });
    }

    let features = db.prepare("SELECT id,title,revision,state,brief_json FROM forge_features WHERE project_id=?1 AND state IN ('incubating','in_triage','ready_for_review') AND NOT EXISTS(SELECT 1 FROM forge_turns t WHERE t.feature_id=forge_features.id AND t.status='running') ORDER BY updated_at,id")?.query_map([project], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,u64>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (id, title, revision, state, brief) in features {
        let brief: serde_json::Value =
            serde_json::from_str(&brief).map_err(|error| invalid(error.to_string()))?;
        let pending = brief
            .get("pendingDecisions")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let (kind, description, actions, decision_id) = if state == "ready_for_review" {
            (
                PendingDecisionKind::Delivery,
                "The feature result is ready for your final review.".into(),
                vec![PendingDecisionAction::OpenDelivery],
                format!("delivery:{id}:{revision}"),
            )
        } else if !pending.is_empty() {
            let latest: Option<String> = db.query_row("SELECT id FROM forge_turns WHERE feature_id=?1 AND status='completed' AND kind='discovery' AND feature_revision+1=?2 ORDER BY ordinal DESC LIMIT 1", rusqlite::params![id,revision], |row|row.get(0)).optional()?;
            (
                PendingDecisionKind::ForgeQuestions,
                pending
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                vec![PendingDecisionAction::OpenForge],
                format!("forge:{id}:{revision}:{}", latest.unwrap_or_default()),
            )
        } else {
            continue;
        };
        let forge_questions = if kind == PendingDecisionKind::ForgeQuestions {
            current_feature_questions_on(db, project, &id, revision)?
        } else {
            None
        };
        decisions.push(PendingDecision {
            id: decision_id,
            project_id: project.into(),
            kind,
            source_id: id.clone(),
            pulsation_id: None,
            checkpoint_id: None,
            feature_id: Some(id),
            input_revision: Some(revision),
            state_version: revision,
            stage: None,
            title: safe(&title, 256),
            description: safe(&description, 4000),
            actions,
            question_set: None,
            forge_questions,
        });
    }
    if let Some((version,reason)) = db.query_row("SELECT state_version,blocking_reason FROM project_pipeline_state WHERE project_id=?1 AND base_state='needs_review'", [project], |row|Ok((row.get::<_,u64>(0)?,row.get::<_,Option<String>>(1)?))).optional()? {
        decisions.push(PendingDecision {id:format!("base:{project}:{version}"),project_id:project.into(),kind:PendingDecisionKind::BaseReview,source_id:project.into(),pulsation_id:None,checkpoint_id:None,feature_id:None,input_revision:None,state_version:version,stage:None,title:"Project reconciliation".into(),description:safe(reason.as_deref().unwrap_or("Review the project base before another dispatch."),4000),actions:vec![PendingDecisionAction::OpenReconciliation],question_set:None,forge_questions:None});
    }
    let descriptions = db.prepare("SELECT p.id,p.feature_name,p.input_revision,p.state_version FROM pulsations p JOIN pulsation_preparation r ON r.pulsation_id=p.id AND r.input_revision=p.input_revision WHERE p.project_id=?1 AND p.status IN ('planning','running') AND p.specification_started=0 AND p.stage IN ('Intake','SPECIFICATION') AND r.review_state='pending' ORDER BY p.queue_sequence")?.query_map([project], |row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,u64>(2)?,row.get::<_,u64>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (id, title, revision, version) in descriptions {
        decisions.push(PendingDecision {
            id: format!("description:{id}:{revision}"),
            project_id: project.into(),
            kind: PendingDecisionKind::DescriptionReview,
            source_id: id.clone(),
            pulsation_id: Some(id),
            checkpoint_id: None,
            feature_id: None,
            input_revision: Some(revision),
            state_version: version,
            stage: None,
            title: safe(&title, 256),
            description: "Confirm the prepared description before Specification starts.".into(),
            actions: vec![PendingDecisionAction::OpenFullReview],
            question_set: None,
            forge_questions: None,
        });
    }
    Ok(decisions)
}

pub(crate) fn read_pending_decision_on(
    db: &Connection,
    project: &str,
    id: &str,
) -> Result<Option<PendingDecision>, PersistenceServiceError> {
    if id.len() > 512 {
        return Err(invalid("Invalid decision reference."));
    }
    Ok(list_pending_decisions_on(db, project)?
        .into_iter()
        .find(|decision| decision.id == id))
}

impl PersistenceService {
    pub fn list_pending_decisions(
        &self,
        project: &str,
    ) -> Result<Vec<PendingDecision>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        let result = list_pending_decisions_on(&tx, project)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn read_pending_decision(
        &self,
        project: &str,
        id: &str,
    ) -> Result<Option<PendingDecision>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        let result = read_pending_decision_on(&tx, project, id)?;
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
#[path = "decisions_tests.rs"]
mod tests;
