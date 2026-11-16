//! Durable host-owned Canvas offers and jobs. All methods are blocking.
//! Staging never enters the drawing library; publication is one SQLite transaction.
use super::authoring::{
    CanvasAuthoringCandidate, CanvasAuthoringSnapshotV1, CanvasAuthoringSource,
    CanvasCompanionTargetSnapshot, MAX_AUTHORING_REQUESTS, MAX_AUTHORING_SESSION_SECONDS,
};
use super::semantics::{CanvasDocumentV1, CanvasProfileV1, CanvasProvenanceV1};
use super::{
    CanvasSceneV1, CanvasViewportV1, CompanionDrawingsService, normalize_scene, sanitize_title,
};
use crate::chat_service::{ChatExecutionTargetRequest, ChatMessage};
use crate::feature_forge::service::check_project;
use crate::persistence_service::{PersistenceService, PersistenceServiceError as Error};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const MAX_TARGET_BYTES: usize = 32 * 1024;

fn invalid(message: &str) -> Error {
    Error::Validation(message.into())
}
fn digest(bytes: &[u8]) -> String {
    crate::lower_hex(&Sha256::digest(bytes))
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        && crate::secret_redactor::redact_secrets(value).is_ok_and(|clean| clean.text == value)
}
fn clock_value(now: u64) -> Result<(), Error> {
    if now > i64::MAX as u64 {
        return Err(invalid("Canvas timestamp is outside the supported range"));
    }
    Ok(())
}
fn clean_text(text: &str) -> Result<String, Error> {
    if text.trim().is_empty()
        || text.chars().count() > super::MAX_LABEL_CHARS
        || text.chars().any(|c| {
            (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                || matches!(c, '\u{fffe}' | '\u{ffff}')
        })
    {
        return Err(invalid(
            "Canvas request text is empty or exceeds its supported limits",
        ));
    }
    let text = crate::secret_redactor::redact_secrets(text)
        .map_err(|_| invalid("Canvas text could not be sanitized"))?
        .text
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if text.chars().count() > super::MAX_LABEL_CHARS {
        return Err(invalid("Sanitized Canvas text exceeds its supported limit"));
    }
    Ok(text)
}
fn encode(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| invalid("Canvas authoring state could not be encoded"))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, Error> {
    serde_json::from_str(value).map_err(|_| invalid("Stored Canvas authoring state is invalid"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasOfferProposalV1 {
    pub profile: CanvasProfileV1,
    pub language: String,
    pub title: String,
    pub request_text: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasAuthoringRequestStateV1 {
    AwaitingAuthorization,
    Declined,
    Running,
    Ready,
    Failed,
    Cancelled,
    Interrupted,
}
impl CanvasAuthoringRequestStateV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingAuthorization => "awaiting_authorization",
            Self::Declined => "declined",
            Self::Running => "running",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "awaiting_authorization" => Ok(Self::AwaitingAuthorization),
            "declined" => Ok(Self::Declined),
            "running" => Ok(Self::Running),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(invalid("Stored Canvas authoring lifecycle is invalid")),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasAuthoringReadyReceiptV1 {
    pub project_id: String,
    pub conversation_id: String,
    pub request_id: String,
    pub job_id: String,
    pub drawing_id: String,
    pub drawing_revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasAuthoringFailureV1 {
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasAuthoringRequestV1 {
    pub request_id: String,
    pub project_id: String,
    pub conversation_id: String,
    pub assistant_message_id: String,
    pub source_sha256: String,
    pub profile: CanvasProfileV1,
    pub language: String,
    pub title: String,
    pub request_text: String,
    pub state: CanvasAuthoringRequestStateV1,
    pub request_revision: u64,
    pub job_id: Option<String>,
    pub ready: Option<CanvasAuthoringReadyReceiptV1>,
    pub failure: Option<CanvasAuthoringFailureV1>,
    pub created_at: u64,
    pub updated_at: u64,
    pub source_available: bool,
    pub drawing_available: bool,
}
/// A trusted worker context, never part of any model tool schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasAuthoringJobContext {
    pub project_id: String,
    pub conversation_id: String,
    pub request_id: String,
    pub job_id: String,
    pub nonce: String,
}
#[derive(Debug, Clone)]
pub struct CanvasAuthoringAdmissionV1 {
    pub request: CanvasAuthoringRequestV1,
    pub source: CanvasAuthoringSource,
    pub target_snapshot: Value,
    pub expires_at: u64,
    pub context: CanvasAuthoringJobContext,
}
#[derive(Clone)]
pub struct CanvasAuthoringStore {
    drawings: CompanionDrawingsService,
    persistence: PersistenceService,
}

struct Stored {
    request: CanvasAuthoringRequestV1,
    source_explanation: String,
    action_id: Option<String>,
    nonce: Option<String>,
    target: Option<String>,
    binding: Option<String>,
    accepted_at: Option<u64>,
    expires_at: Option<u64>,
    stage_revision: u64,
    stage_scene: Option<String>,
    stage_sha256: Option<String>,
    publication_sha256: Option<String>,
}
fn row_record(row: &Row<'_>) -> rusqlite::Result<Stored> {
    // Lifecycle/profile/receipt decoding happens outside the SQLite row closure.
    let state: String = row.get("state")?;
    let profile: String = row.get("profile")?;
    let state =
        CanvasAuthoringRequestStateV1::parse(&state).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let profile = match profile.as_str() {
        "architecture" => CanvasProfileV1::Architecture,
        "bpmn" => CanvasProfileV1::Bpmn,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let job_id: Option<String> = row.get("job_id")?;
    let drawing_id: Option<String> = row.get("drawing_id")?;
    let drawing_revision: Option<u64> = row.get("drawing_revision")?;
    let project_id: String = row.get("project_id")?;
    let conversation_id: String = row.get("conversation_id")?;
    let request_id: String = row.get("id")?;
    let ready = match (drawing_id, drawing_revision, job_id.as_ref()) {
        (Some(drawing_id), Some(drawing_revision), Some(job_id)) => {
            Some(CanvasAuthoringReadyReceiptV1 {
                project_id: project_id.clone(),
                conversation_id: conversation_id.clone(),
                request_id: request_id.clone(),
                job_id: job_id.clone(),
                drawing_id,
                drawing_revision,
            })
        }
        _ => None,
    };
    let code: Option<String> = row.get("failure_code")?;
    let message: Option<String> = row.get("failure_message")?;
    Ok(Stored {
        request: CanvasAuthoringRequestV1 {
            request_id,
            project_id,
            conversation_id,
            assistant_message_id: row.get("assistant_message_id")?,
            source_sha256: row.get("source_sha256")?,
            profile,
            language: row.get("language")?,
            title: row.get("title")?,
            request_text: row.get("request_text")?,
            state,
            request_revision: row.get("request_revision")?,
            job_id,
            ready,
            failure: code
                .zip(message)
                .map(|(code, message)| CanvasAuthoringFailureV1 { code, message }),
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            source_available: false,
            drawing_available: false,
        },
        source_explanation: row.get("source_explanation")?,
        action_id: row.get("action_id")?,
        nonce: row.get("context_nonce")?,
        target: row.get("target_snapshot_json")?,
        binding: row.get("target_binding_json")?,
        accepted_at: row.get("accepted_at")?,
        expires_at: row.get("expires_at")?,
        stage_revision: row.get("stage_revision")?,
        stage_scene: row.get("stage_scene_json")?,
        stage_sha256: row.get("stage_sha256")?,
        publication_sha256: row.get("publication_sha256")?,
    })
}
fn read_message(
    db: &Connection,
    project: &str,
    conversation: &str,
    message: &str,
) -> Result<Option<ChatMessage>, Error> {
    Ok(db.query_row("SELECT m.id,m.conversation_id,m.ordinal,m.role,m.content,m.status,m.created_at,m.updated_at FROM chat_messages m JOIN chat_conversations c ON c.id=m.conversation_id WHERE c.project_id=?1 AND c.id=?2 AND m.id=?3",params![project,conversation,message],|r|Ok(ChatMessage{id:r.get(0)?,conversation_id:r.get(1)?,ordinal:r.get(2)?,role:r.get(3)?,content:r.get(4)?,status:r.get(5)?,execution_id:None,generation_id:None,last_sequence:0,input_tokens:None,output_tokens:None,estimated_cost_usd:None,error_code:None,error_message:None,target_label:None,model_evidence:None,created_at:r.get(6)?,updated_at:r.get(7)?})).optional()?)
}
fn source(
    db: &Connection,
    request: &CanvasAuthoringRequestV1,
) -> Result<CanvasAuthoringSource, Error> {
    let message = read_message(
        db,
        &request.project_id,
        &request.conversation_id,
        &request.assistant_message_id,
    )?
    .ok_or_else(|| invalid("Canvas explanation source is unavailable"))?;
    if digest(message.content.as_bytes()) != request.source_sha256 {
        return Err(invalid(
            "Canvas explanation source changed after the request",
        ));
    }
    CanvasAuthoringSource::capture_final_message(
        &request.project_id,
        &message,
        &request.language,
        None,
    )
}
fn enrich(db: &Connection, mut stored: Stored) -> Result<Stored, Error> {
    stored.request.source_available = read_message(
        db,
        &stored.request.project_id,
        &stored.request.conversation_id,
        &stored.request.assistant_message_id,
    )?
    .is_some_and(|message| {
        message.role == "assistant"
            && message.status == "completed"
            && digest(message.content.as_bytes()) == stored.request.source_sha256
    });
    if let Some(receipt) = &stored.request.ready {
        stored.request.drawing_available = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM companion_drawings WHERE project_id=?1 AND id=?2)",
            params![receipt.project_id, receipt.drawing_id],
            |r| r.get(0),
        )?;
    }
    Ok(stored)
}
fn load(
    db: &Connection,
    project: &str,
    conversation: &str,
    request: &str,
) -> Result<Stored, Error> {
    let stored=db.query_row("SELECT * FROM companion_canvas_requests WHERE project_id=?1 AND conversation_id=?2 AND id=?3",params![project,conversation,request],row_record).optional()?.ok_or_else(||Error::NotFound("Canvas authoring request".into()))?;
    enrich(db, stored)
}
fn check_context(stored: &Stored, context: &CanvasAuthoringJobContext) -> Result<(), Error> {
    if stored.request.project_id != context.project_id
        || stored.request.conversation_id != context.conversation_id
        || stored.request.request_id != context.request_id
        || stored.request.job_id.as_deref() != Some(context.job_id.as_str())
        || stored.nonce.as_deref() != Some(context.nonce.as_str())
    {
        return Err(invalid(
            "Canvas worker context does not match its admitted job",
        ));
    }
    Ok(())
}
fn check_running(stored: &Stored, now: u64) -> Result<(), Error> {
    clock_value(now)?;
    if stored.request.state != CanvasAuthoringRequestStateV1::Running
        || stored.accepted_at.is_none_or(|accepted| now < accepted)
        || stored.expires_at.is_none_or(|expires| now >= expires)
    {
        return Err(invalid("Canvas authoring job is inactive or expired"));
    }
    Ok(())
}
fn next_revision(stored: &Stored) -> Result<u64, Error> {
    stored
        .request
        .request_revision
        .checked_add(1)
        .filter(|revision| *revision <= i64::MAX as u64)
        .ok_or_else(|| invalid("Canvas request revision limit reached"))
}
fn binding(target: &CanvasCompanionTargetSnapshot) -> Result<String, Error> {
    encode(&serde_json::json!({"target":target.target(),"model_id":target.model_id()}))
}
type ConversationTargetRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
);

fn check_current_target(
    db: &Connection,
    project: &str,
    conversation: &str,
    expected: &CanvasCompanionTargetSnapshot,
) -> Result<(), Error> {
    let current:Option<ConversationTargetRow>=db.query_row("SELECT transport_kind,connection_id,model_id_snapshot,cli_backend,model_profile FROM chat_conversations WHERE project_id=?1 AND id=?2",params![project,conversation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((transport, connection, model, backend, profile)) = current else {
        return Err(invalid("Canvas conversation is unavailable"));
    };
    let matches = match expected.target() {
        ChatExecutionTargetRequest::Api { connection_id } => {
            transport == "api" && connection.as_ref() == Some(connection_id)
        }
        ChatExecutionTargetRequest::Cli {
            backend: expected_backend,
            model: expected_model,
            model_profile,
        } => {
            transport == "cli"
                && backend.as_deref() == Some(expected_backend.as_str())
                && model == expected_model.as_deref().unwrap_or("default")
                && &profile == model_profile
        }
    };
    if !matches || model != expected.model_id() {
        return Err(invalid(
            "The Companion target changed; review the Canvas request again",
        ));
    }
    Ok(())
}
fn safe_target(value: &Value, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    match value {
        Value::Object(fields) => {
            fields.len() <= 32
                && fields.iter().all(|(key, value)| {
                    !matches!(
                        key.to_ascii_lowercase().as_str(),
                        "api_key"
                            | "apikey"
                            | "access_token"
                            | "token"
                            | "authorization"
                            | "credentials"
                            | "credential"
                            | "credential_id"
                            | "credential_ref"
                            | "secret"
                            | "password"
                            | "headers"
                            | "body"
                            | "payload"
                    ) && safe_target(value, depth + 1)
                })
        }
        Value::Array(values) => {
            values.len() <= 32 && values.iter().all(|value| safe_target(value, depth + 1))
        }
        Value::String(text) => text.len() <= 1024 && !text.chars().any(char::is_control),
        _ => true,
    }
}
fn target_json(value: &Value) -> Result<String, Error> {
    let encoded = encode(value)?;
    if !value.is_object()
        || encoded.len() > MAX_TARGET_BYTES
        || !safe_target(value, 0)
        || !crate::secret_redactor::redact_secrets(&encoded)
            .is_ok_and(|clean| clean.text == encoded)
    {
        return Err(invalid(
            "Canvas target snapshot exceeds its metadata boundary",
        ));
    }
    Ok(encoded)
}
fn stage_for_request(
    scene: &CanvasSceneV1,
    request: &CanvasAuthoringRequestV1,
) -> Result<CanvasSceneV1, Error> {
    let scene = normalize_scene(scene)?;
    let document = scene
        .document
        .as_ref()
        .ok_or_else(|| invalid("Canvas authoring requires an explicit document profile"))?;
    let expected = CanvasProvenanceV1 {
        source_id: request.assistant_message_id.clone(),
        source_revision: None,
        source_sha256: request.source_sha256.clone(),
    };
    if document.profile != request.profile
        || document.language != request.language
        || document.provenance.as_ref() != Some(&expected)
    {
        return Err(invalid(
            "Canvas staging cannot replace its admitted profile, language or provenance",
        ));
    }
    Ok(scene)
}

impl CanvasAuthoringStore {
    pub fn new(drawings: CompanionDrawingsService) -> Self {
        Self {
            persistence: drawings.persistence.clone(),
            drawings,
        }
    }

    /// One existing journal-backed mirror pass; callers must not hold the project guard.
    pub fn sync_project(&self, project: &str) -> Result<super::SyncReport, Error> {
        self.drawings.sync_project(project)
    }

    pub fn propose(
        &self,
        project: &str,
        conversation: &str,
        assistant_message_id: &str,
        proposal: &CanvasOfferProposalV1,
        now: u64,
    ) -> Result<CanvasAuthoringRequestV1, Error> {
        clock_value(now)?;
        if ![project, conversation, assistant_message_id]
            .into_iter()
            .all(valid_id)
        {
            return Err(invalid("Canvas proposal identity is invalid"));
        }
        let title = sanitize_title(&proposal.title)?;
        let request_text = clean_text(&proposal.request_text)?;
        normalize_scene(&CanvasSceneV1 {
            document: Some(CanvasDocumentV1 {
                schema_version: 1,
                profile: proposal.profile,
                language: proposal.language.clone(),
                narrative: String::new(),
                provenance: None,
                elements: vec![],
            }),
            ..Default::default()
        })?;
        self.drawings.with_project_guard(project,||{
            let mut db=self.persistence.connection()?;let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;check_project(&tx,project)?;
            let message=read_message(&tx,project,conversation,assistant_message_id)?.ok_or_else(||invalid("Canvas explanation source is unavailable"))?;
            let source=CanvasAuthoringSource::capture_final_message(project,&message,&proposal.language,None)?;
            let previous:Option<String>=tx.query_row("SELECT id FROM companion_canvas_requests WHERE project_id=?1 AND assistant_message_id=?2",params![project,assistant_message_id],|r|r.get(0)).optional()?;
            if let Some(id)=previous{
                let old=load(&tx,project,conversation,&id)?;
                if old.request.profile!=proposal.profile||old.request.language!=proposal.language||old.request.title!=title||old.request.request_text!=request_text||old.request.source_sha256!=source.source_sha256(){return Err(invalid("Canvas source already has a different authoring request"));}
                return Ok(old.request);
            }
            let id=Uuid::new_v4().to_string();let profile=match proposal.profile{CanvasProfileV1::Architecture=>"architecture",CanvasProfileV1::Bpmn=>"bpmn"};
            tx.execute("INSERT INTO companion_canvas_requests(id,project_id,conversation_id,assistant_message_id,source_sha256,source_explanation,profile,language,title,request_text,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'awaiting_authorization',?11,?11)",params![id,project,conversation,assistant_message_id,source.source_sha256(),source.explanation(),profile,proposal.language,title,request_text,now])?;
            let result=load(&tx,project,conversation,&id)?.request;tx.commit()?;Ok(result)
        })
    }

    pub fn list(
        &self,
        project: &str,
        conversation: &str,
    ) -> Result<Vec<CanvasAuthoringRequestV1>, Error> {
        let db = self.persistence.connection()?;
        check_project(&db, project)?;
        let mut statement=db.prepare("SELECT * FROM companion_canvas_requests WHERE project_id=?1 AND conversation_id=?2 ORDER BY created_at DESC,rowid DESC LIMIT 1000")?;
        let mut records = statement
            .query_map(params![project, conversation], row_record)?
            .collect::<Result<Vec<_>, _>>()?;
        records.reverse();
        records
            .into_iter()
            .map(|record| Ok(enrich(&db, record)?.request))
            .collect()
    }
    pub fn get(
        &self,
        project: &str,
        conversation: &str,
        request: &str,
    ) -> Result<CanvasAuthoringRequestV1, Error> {
        let db = self.persistence.connection()?;
        check_project(&db, project)?;
        Ok(load(&db, project, conversation, request)?.request)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admit(
        &self,
        project: &str,
        conversation: &str,
        request: &str,
        expected_request_revision: u64,
        action_id: &str,
        expected_target: &CanvasCompanionTargetSnapshot,
        target_snapshot: &Value,
        now: u64,
        expires_at: u64,
    ) -> Result<CanvasAuthoringAdmissionV1, Error> {
        clock_value(now)?;
        clock_value(expires_at)?;
        if !valid_id(action_id)
            || expires_at <= now
            || expires_at - now > MAX_AUTHORING_SESSION_SECONDS
        {
            return Err(invalid("Canvas acceptance identity or lifetime is invalid"));
        }
        let target = target_json(target_snapshot)?;
        let target_binding = binding(expected_target)?;
        self.drawings.with_project_guard(project,||{
            let mut db=self.persistence.connection()?;let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;check_project(&tx,project)?;
            let mut stored=load(&tx,project,conversation,request)?;
            let captured=source(&tx,&stored.request)?;
            if captured.explanation()!=stored.source_explanation{return Err(invalid("Stored Canvas source snapshot does not match the completed explanation"));}
            if stored.action_id.as_deref()==Some(action_id)&&matches!(stored.request.state,CanvasAuthoringRequestStateV1::Running|CanvasAuthoringRequestStateV1::Ready){
                if stored.target.as_deref()!=Some(target.as_str())||stored.binding.as_deref()!=Some(target_binding.as_str()){
                    return Err(invalid("Canvas approval replay cannot replace its captured target"));
                }
                return admission(stored,captured);
            }
            if stored.request.state!=CanvasAuthoringRequestStateV1::AwaitingAuthorization||stored.request.request_revision!=expected_request_revision{return Err(invalid("Canvas request changed or is no longer awaiting authorization"));}
            check_current_target(&tx,project,conversation,expected_target)?;
            let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM companion_canvas_requests WHERE conversation_id=?1 AND state='running')",[conversation],|r|r.get(0))?;
            if active{return Err(invalid("Another Canvas authoring job is already running in this conversation"));}
            let revision=next_revision(&stored)?;let job=Uuid::new_v4().to_string();let nonce=Uuid::new_v4().to_string();
            tx.execute("UPDATE companion_canvas_requests SET state='running',request_revision=?1,job_id=?2,action_id=?3,context_nonce=?4,target_snapshot_json=?5,target_binding_json=?6,accepted_at=?7,expires_at=?8,updated_at=?7 WHERE id=?9",params![revision,job,action_id,nonce,target,target_binding,now,expires_at,request])?;
            stored=load(&tx,project,conversation,request)?;let admitted=admission(stored,captured)?;tx.commit()?;Ok(admitted)
        })
    }

    pub fn decline(
        &self,
        project: &str,
        conversation: &str,
        request: &str,
        expected_revision: u64,
        now: u64,
    ) -> Result<CanvasAuthoringRequestV1, Error> {
        self.end_from_ui(project, conversation, request, expected_revision, now, true)
    }
    pub fn cancel(
        &self,
        project: &str,
        conversation: &str,
        request: &str,
        expected_revision: u64,
        now: u64,
    ) -> Result<CanvasAuthoringRequestV1, Error> {
        self.end_from_ui(
            project,
            conversation,
            request,
            expected_revision,
            now,
            false,
        )
    }
    fn end_from_ui(
        &self,
        project: &str,
        conversation: &str,
        request: &str,
        expected_revision: u64,
        now: u64,
        decline: bool,
    ) -> Result<CanvasAuthoringRequestV1, Error> {
        clock_value(now)?;
        self.drawings.with_project_guard(project,||{
            let mut db=self.persistence.connection()?;let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;check_project(&tx,project)?;let stored=load(&tx,project,conversation,request)?;
            if !matches!(stored.request.state,CanvasAuthoringRequestStateV1::AwaitingAuthorization|CanvasAuthoringRequestStateV1::Running){return Ok(stored.request);}
            if stored.request.request_revision!=expected_revision||decline&&stored.request.state!=CanvasAuthoringRequestStateV1::AwaitingAuthorization{return Err(invalid("Canvas request revision or action conflicts with its state"));}
            let state=if decline{"declined"}else{"cancelled"};tx.execute("UPDATE companion_canvas_requests SET state=?1,request_revision=?2,stage_scene_json=NULL,stage_sha256=NULL,updated_at=?3 WHERE id=?4",params![state,next_revision(&stored)?,now,request])?;
            let result=load(&tx,project,conversation,request)?.request;tx.commit()?;Ok(result)
        })
    }

    pub fn record_progress(
        &self,
        context: &CanvasAuthoringJobContext,
        snapshot: &CanvasAuthoringSnapshotV1,
        now: u64,
    ) -> Result<CanvasAuthoringRequestV1, Error> {
        self.drawings.with_project_guard(&context.project_id,||{
            let mut db=self.persistence.connection()?;let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;check_project(&tx,&context.project_id)?;
            let stored=load(&tx,&context.project_id,&context.conversation_id,&context.request_id)?;check_context(&stored,context)?;check_running(&stored,now)?;
            if snapshot.revision>MAX_AUTHORING_REQUESTS as u64||snapshot.revision<stored.stage_revision||snapshot.revision>stored.stage_revision+1{return Err(invalid("Canvas staged revision conflicts with recorded progress"));}
            let scene=stage_for_request(&snapshot.scene,&stored.request)?;let json=encode(&scene)?;let hash=digest(json.as_bytes());
            if snapshot.revision==stored.stage_revision&&stored.stage_scene.is_some(){if stored.stage_sha256.as_deref()==Some(hash.as_str()){return Ok(stored.request);}return Err(invalid("Canvas staged revision was reused for different content"));}
            tx.execute("UPDATE companion_canvas_requests SET stage_revision=?1,stage_scene_json=?2,stage_sha256=?3,updated_at=?4 WHERE id=?5",params![snapshot.revision,json,hash,now,context.request_id])?;
            let result=load(&tx,&context.project_id,&context.conversation_id,&context.request_id)?.request;tx.commit()?;Ok(result)
        })
    }

    pub fn fail(
        &self,
        context: &CanvasAuthoringJobContext,
        failure: &CanvasAuthoringFailureV1,
        now: u64,
    ) -> Result<CanvasAuthoringRequestV1, Error> {
        clock_value(now)?;
        if !valid_id(&failure.code) || failure.code.len() > 64 {
            return Err(invalid("Canvas failure code is invalid"));
        }
        let message = clean_text(&failure.message)?;
        self.drawings.with_project_guard(&context.project_id,||{
            let mut db=self.persistence.connection()?;let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;check_project(&tx,&context.project_id)?;let stored=load(&tx,&context.project_id,&context.conversation_id,&context.request_id)?;check_context(&stored,context)?;
            if stored.request.state!=CanvasAuthoringRequestStateV1::Running{return Ok(stored.request);}
            tx.execute("UPDATE companion_canvas_requests SET state='failed',request_revision=?1,failure_code=?2,failure_message=?3,stage_scene_json=NULL,stage_sha256=NULL,updated_at=?4 WHERE id=?5",params![next_revision(&stored)?,failure.code,message,now,context.request_id])?;
            let result=load(&tx,&context.project_id,&context.conversation_id,&context.request_id)?.request;tx.commit()?;Ok(result)
        })
    }

    pub fn publish(
        &self,
        context: &CanvasAuthoringJobContext,
        candidate: &CanvasAuthoringCandidate,
        now: u64,
    ) -> Result<CanvasAuthoringRequestV1, Error> {
        clock_value(now)?;
        let result=self.drawings.with_project_guard(&context.project_id,||{
            let mut db=self.persistence.connection()?;let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;check_project(&tx,&context.project_id)?;
            let stored=load(&tx,&context.project_id,&context.conversation_id,&context.request_id)?;check_context(&stored,context)?;
            let scene=stage_for_request(&candidate.scene,&stored.request)?;let title=sanitize_title(&candidate.title)?;
            if candidate.session_id!=context.job_id||title!=stored.request.title||binding(&candidate.target)?!=stored.binding.as_deref().unwrap_or(""){return Err(invalid("Canvas candidate does not match its admitted job"));}
            let scene_json=encode(&scene)?;let candidate_hash=digest(encode(&(candidate.session_id.as_str(),title.as_str(),candidate.revision,scene_json.as_str(),candidate.source.source_sha256()))?.as_bytes());
            if stored.request.state==CanvasAuthoringRequestStateV1::Ready {
                if stored.publication_sha256.as_deref()!=Some(candidate_hash.as_str()){return Err(invalid("Canvas publication was already finalized for a different candidate"));}
                return Ok(stored.request);
            }
            check_running(&stored,now)?;let captured=source(&tx,&stored.request)?;
            if candidate.source!=captured||candidate.revision!=stored.stage_revision||stored.stage_scene.as_deref()!=Some(scene_json.as_str())||scene.elements.is_empty(){return Err(invalid("Canvas candidate is stale, incomplete or different from its recorded staging"));}
            let drawing=super::service::insert_drawing(&tx,&context.project_id,&title,&scene,&CanvasViewportV1::default())?;
            tx.execute("UPDATE companion_canvas_requests SET state='ready',request_revision=?1,drawing_id=?2,drawing_revision=?3,publication_sha256=?4,stage_scene_json=NULL,stage_sha256=NULL,failure_code=NULL,failure_message=NULL,updated_at=?5 WHERE id=?6",params![next_revision(&stored)?,drawing.id,drawing.revision,candidate_hash,now,context.request_id])?;
            let result=load(&tx,&context.project_id,&context.conversation_id,&context.request_id)?.request;tx.commit()?;Ok(result)
        })?;
        if let Some(receipt) = &result.ready {
            self.drawings.changed(
                &receipt.project_id,
                &receipt.drawing_id,
                receipt.drawing_revision,
            );
        }
        Ok(result)
    }

    pub fn cancel_project(&self, project: &str, now: u64) -> Result<usize, Error> {
        clock_value(now)?;
        self.drawings.with_project_guard(project,||{
            let db=self.persistence.connection()?;Ok(db.execute("UPDATE companion_canvas_requests SET state='cancelled',request_revision=request_revision+1,stage_scene_json=NULL,stage_sha256=NULL,updated_at=?1 WHERE project_id=?2 AND state IN ('awaiting_authorization','running')",params![now,project])?)
        })
    }
    pub fn recover_interrupted(&self, now: u64) -> Result<usize, Error> {
        clock_value(now)?;
        let db = self.persistence.connection()?;
        let projects = db
            .prepare(
                "SELECT DISTINCT project_id FROM companion_canvas_requests WHERE state='running'",
            )?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut count = 0;
        for project in projects {
            count+=self.drawings.with_project_guard(&project,||{
            let db=self.persistence.connection()?;Ok(db.execute("UPDATE companion_canvas_requests SET state='interrupted',request_revision=request_revision+1,stage_scene_json=NULL,stage_sha256=NULL,failure_code='interrupted',failure_message='Canvas generation was interrupted; no drawing was published.',updated_at=?1 WHERE project_id=?2 AND state='running'",params![now,project])?)
        })?;
        }
        Ok(count)
    }
}

fn admission(
    stored: Stored,
    source: CanvasAuthoringSource,
) -> Result<CanvasAuthoringAdmissionV1, Error> {
    let job_id = stored
        .request
        .job_id
        .clone()
        .ok_or_else(|| invalid("Stored Canvas job identity is unavailable"))?;
    let nonce = stored
        .nonce
        .ok_or_else(|| invalid("Stored Canvas worker context is unavailable"))?;
    let context = CanvasAuthoringJobContext {
        project_id: stored.request.project_id.clone(),
        conversation_id: stored.request.conversation_id.clone(),
        request_id: stored.request.request_id.clone(),
        job_id,
        nonce,
    };
    let target_snapshot: Value = decode(
        stored
            .target
            .as_deref()
            .ok_or_else(|| invalid("Stored Canvas target is unavailable"))?,
    )?;
    target_json(&target_snapshot)?;
    Ok(CanvasAuthoringAdmissionV1 {
        request: stored.request,
        source,
        target_snapshot,
        expires_at: stored
            .expires_at
            .ok_or_else(|| invalid("Stored Canvas expiry is unavailable"))?,
        context,
    })
}

#[cfg(test)]
#[path = "authoring_store_tests.rs"]
mod tests;
