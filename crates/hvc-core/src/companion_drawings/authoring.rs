//! Pure staged Canvas authoring contracts; no runtime or publication side effects.

use super::semantics::{
    CanvasDiagnosticV1, CanvasDocumentV1, CanvasProfileV1, CanvasProvenanceV1,
    CanvasSemanticElementV1, validate_document,
};
use super::{CanvasElementKindV1, CanvasElementV1, CanvasSceneV1, normalize_scene, sanitize_title};
use crate::chat_service::{ChatExecutionTargetRequest, ChatMessage};
use crate::persistence_service::PersistenceServiceError as Error;
use crate::terminals::CliBackend;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

pub const MAX_AUTHORING_SOURCE_BYTES: usize = 64 * 1024;
pub const MAX_AUTHORING_SOURCE_CHARS: usize = 16_000;
pub const MAX_AUTHORING_SESSION_SECONDS: u64 = 15 * 60;
pub const MAX_AUTHORING_COMMANDS: usize = 128;
pub const MAX_AUTHORING_BATCH_BYTES: usize = 256 * 1024;
pub const MAX_AUTHORING_REQUESTS: usize = 128;

fn invalid(message: &str) -> Error {
    Error::Validation(message.into())
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        && crate::secret_redactor::redact_secrets(value).is_ok_and(|clean| clean.text == value)
}

fn bounded_value(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn clean_explanation(value: &str) -> Result<String, Error> {
    if value.trim().is_empty()
        || value.len() > MAX_AUTHORING_SOURCE_BYTES
        || value.chars().count() > MAX_AUTHORING_SOURCE_CHARS
        || value.chars().any(|c| {
            (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                || matches!(c, '\u{fffe}' | '\u{ffff}')
        })
    {
        return Err(invalid(
            "Canvas explanation is empty or outside the supported text limits",
        ));
    }
    let text = crate::secret_redactor::redact_secrets(value)
        .map_err(|_| invalid("Canvas explanation could not be sanitized"))?
        .text;
    if text.len() > MAX_AUTHORING_SOURCE_BYTES || text.chars().count() > MAX_AUTHORING_SOURCE_CHARS
    {
        return Err(invalid(
            "Sanitized Canvas explanation exceeds the supported text limits",
        ));
    }
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn fingerprint(value: &[u8]) -> String {
    crate::lower_hex(&Sha256::digest(value))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasSourceDocumentV1 {
    pub document_id: String,
    /// An opaque source revision, never a path to be opened by this module.
    pub revision: String,
}

/// A host-captured completed answer. It is deliberately not deserializable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasAuthoringSource {
    project_id: String,
    conversation_id: String,
    final_message_id: String,
    source_sha256: String,
    explanation: String,
    language: String,
    document: Option<CanvasSourceDocumentV1>,
}

impl CanvasAuthoringSource {
    /// The host must resolve the message and optional document within `project_id`.
    /// This pure snapshot validates finality; it performs no repository lookup.
    pub fn capture_final_message(
        project_id: &str,
        message: &ChatMessage,
        language: &str,
        document: Option<CanvasSourceDocumentV1>,
    ) -> Result<Self, Error> {
        if !valid_id(project_id)
            || !valid_id(&message.conversation_id)
            || !valid_id(&message.id)
            || message.role != "assistant"
            || message.status != "completed"
        {
            return Err(invalid(
                "Canvas source must be a host-resolved completed assistant message",
            ));
        }
        if language.is_empty()
            || language.len() > 35
            || !language
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(invalid("Canvas source language is invalid"));
        }
        if document
            .as_ref()
            .is_some_and(|doc| !valid_id(&doc.document_id) || !bounded_value(&doc.revision, 256))
        {
            return Err(invalid(
                "Canvas document source identity or revision is invalid",
            ));
        }
        let explanation = clean_explanation(&message.content)?;
        Ok(Self {
            project_id: project_id.into(),
            conversation_id: message.conversation_id.clone(),
            final_message_id: message.id.clone(),
            source_sha256: fingerprint(message.content.as_bytes()),
            explanation,
            language: language.into(),
            document,
        })
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }
    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }
    pub fn final_message_id(&self) -> &str {
        &self.final_message_id
    }
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    pub fn explanation(&self) -> &str {
        &self.explanation
    }
    pub fn language(&self) -> &str {
        &self.language
    }
    pub fn document(&self) -> Option<&CanvasSourceDocumentV1> {
        self.document.as_ref()
    }
}

/// A frozen Companion selection, not a separately configured drawing route.
/// This pure contract captures the connection ID and model, not resolved provider
/// identity or credentials. Runtime admission must resolve and freeze those too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasCompanionTargetSnapshot {
    target: ChatExecutionTargetRequest,
    model_id: String,
}

impl CanvasCompanionTargetSnapshot {
    pub fn capture_from_host(
        target: ChatExecutionTargetRequest,
        model_id: &str,
    ) -> Result<Self, Error> {
        if !bounded_value(model_id, 256) {
            return Err(invalid("Canvas Companion model snapshot is invalid"));
        }
        match &target {
            ChatExecutionTargetRequest::Api { connection_id } if !valid_id(connection_id) => {
                return Err(invalid("Canvas Companion connection snapshot is invalid"));
            }
            ChatExecutionTargetRequest::Cli {
                backend,
                model,
                model_profile,
            } if *backend == CliBackend::Custom
                || (*backend == CliBackend::Gemini && model_profile.is_some())
                || model.as_deref().unwrap_or("default") != model_id
                || model.as_ref().is_some_and(|value| {
                    !bounded_value(value, 200)
                        || !value.bytes().all(|character| {
                            character.is_ascii_alphanumeric()
                                || matches!(
                                    character,
                                    b'-' | b'_' | b'.' | b':' | b'/' | b'@' | b'+'
                                )
                        })
                })
                || model_profile.as_ref().is_some_and(|value| {
                    !bounded_value(value, 400) || value.chars().count() > 100
                }) =>
            {
                return Err(invalid(
                    "Canvas Companion CLI snapshot is unsupported or inconsistent",
                ));
            }
            _ => {}
        }
        Ok(Self {
            target,
            model_id: model_id.into(),
        })
    }

    pub fn target(&self) -> &ChatExecutionTargetRequest {
        &self.target
    }
    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// Shared geometry/semantics commands for staging and later manual editing.
/// Authority, profile, language and provenance are never command fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CanvasCommandV1 {
    UpsertElement {
        element: Box<CanvasElementV1>,
        semantic: Option<CanvasSemanticElementV1>,
    },
    RemoveElement {
        element_id: String,
    },
    SetNarrative {
        narrative: String,
    },
}

fn check_commands(commands: &[CanvasCommandV1]) -> Result<(), Error> {
    if commands.is_empty() || commands.len() > MAX_AUTHORING_COMMANDS {
        return Err(invalid("Canvas batch must contain 1–128 commands"));
    }
    for command in commands {
        match command {
            CanvasCommandV1::UpsertElement { element, semantic } => {
                if !valid_id(&element.id)
                    || element.label.chars().count() > super::MAX_LABEL_CHARS
                    || semantic.as_ref().is_some_and(|value| {
                        !valid_id(&value.element_id)
                            || [
                                &value.participant_id,
                                &value.lane_id,
                                &value.source_id,
                                &value.target_id,
                            ]
                            .into_iter()
                            .flatten()
                            .any(|id| !valid_id(id))
                    })
                {
                    return Err(invalid(
                        "Canvas element command exceeds its supported field limits",
                    ));
                }
            }
            CanvasCommandV1::RemoveElement { element_id } if !valid_id(element_id) => {
                return Err(invalid("Canvas removal identity is invalid"));
            }
            CanvasCommandV1::SetNarrative { narrative }
                if narrative.len() > MAX_AUTHORING_SOURCE_BYTES
                    || narrative.chars().count() > super::MAX_LABEL_CHARS =>
            {
                return Err(invalid(
                    "Canvas narrative exceeds its supported text limits",
                ));
            }
            _ => {}
        }
    }
    if serde_json::to_vec(commands)
        .map_err(|_| invalid("Canvas commands could not be encoded"))?
        .len()
        > MAX_AUTHORING_BATCH_BYTES
    {
        return Err(invalid("Canvas command batch exceeds 256 KiB"));
    }
    Ok(())
}

/// Clone, apply and normalize once. A rejected batch never mutates its input.
pub fn apply_canvas_commands(
    scene: &CanvasSceneV1,
    commands: &[CanvasCommandV1],
) -> Result<CanvasSceneV1, Error> {
    check_commands(commands)?;
    let mut candidate = scene.clone();
    for command in commands {
        match command {
            CanvasCommandV1::UpsertElement { element, semantic } => {
                if semantic
                    .as_ref()
                    .is_some_and(|value| value.element_id != element.id)
                {
                    return Err(invalid(
                        "Canvas geometry and semantic identities must match",
                    ));
                }
                if let Some(semantic) = semantic {
                    let document = candidate.document.as_mut().ok_or_else(|| {
                        invalid("Legacy drawings cannot infer a semantic profile")
                    })?;
                    if let Some(existing) = document
                        .elements
                        .iter_mut()
                        .find(|value| value.element_id == semantic.element_id)
                    {
                        *existing = semantic.clone();
                    } else {
                        document.elements.push(semantic.clone());
                    }
                }
                if let Some(existing) = candidate
                    .elements
                    .iter_mut()
                    .find(|value| value.id == element.id)
                {
                    *existing = element.as_ref().clone();
                } else {
                    candidate.elements.push(element.as_ref().clone());
                }
            }
            CanvasCommandV1::RemoveElement { element_id } => {
                if !candidate
                    .elements
                    .iter()
                    .any(|value| value.id == *element_id)
                {
                    return Err(invalid("Canvas removal target does not exist"));
                }
                let mut removed = HashSet::from([element_id.clone()]);
                if let Some(document) = &candidate.document {
                    for element in &document.elements {
                        if element.source_id.as_ref() == Some(element_id)
                            || element.target_id.as_ref() == Some(element_id)
                        {
                            removed.insert(element.element_id.clone());
                        }
                    }
                }
                for element in &candidate.elements {
                    if let CanvasElementKindV1::Arrow {
                        start_binding,
                        end_binding,
                        ..
                    } = &element.shape
                        && [start_binding, end_binding]
                            .into_iter()
                            .flatten()
                            .any(|binding| binding.element_id == *element_id)
                    {
                        removed.insert(element.id.clone());
                    }
                }
                candidate
                    .elements
                    .retain(|value| !removed.contains(&value.id));
                for element in &mut candidate.elements {
                    if element
                        .container_id
                        .as_ref()
                        .is_some_and(|id| removed.contains(id))
                    {
                        element.container_id = None;
                    }
                }
                if let Some(document) = &mut candidate.document {
                    document
                        .elements
                        .retain(|value| !removed.contains(&value.element_id));
                }
            }
            CanvasCommandV1::SetNarrative { narrative } => {
                candidate
                    .document
                    .as_mut()
                    .ok_or_else(|| invalid("Legacy drawings have no semantic narrative"))?
                    .narrative = narrative.clone();
            }
        }
    }
    normalize_scene(&candidate)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasAuthoringBatchV1 {
    pub schema_version: u32,
    pub operation_id: String,
    pub expected_revision: u64,
    pub commands: Vec<CanvasCommandV1>,
}

impl CanvasAuthoringBatchV1 {
    pub fn parse_json(json: &str) -> Result<Self, Error> {
        if json.len() > MAX_AUTHORING_BATCH_BYTES {
            return Err(invalid("Canvas authoring request exceeds 256 KiB"));
        }
        let batch: Self = serde_json::from_str(json)
            .map_err(|_| invalid("Canvas authoring request has an invalid or unknown field"))?;
        batch.validate_limits()?;
        Ok(batch)
    }

    fn validate_limits(&self) -> Result<String, Error> {
        if self.schema_version != 1 || !valid_id(&self.operation_id) {
            return Err(invalid(
                "Canvas authoring schema version or operation identity is invalid",
            ));
        }
        check_commands(&self.commands)?;
        let canonical = serde_json::to_vec(self)
            .map_err(|_| invalid("Canvas authoring request could not be encoded"))?;
        if canonical.len() > MAX_AUTHORING_BATCH_BYTES {
            return Err(invalid("Canvas authoring request exceeds 256 KiB"));
        }
        Ok(fingerprint(&canonical))
    }
}

/// Trusted invocation context. No model schema deserializes this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasAuthoringContext {
    project_id: String,
    conversation_id: String,
    session_id: String,
    origin: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasAuthoringState {
    Active,
    Cancelled,
    Closed,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanvasAuthoringBatchReceiptV1 {
    pub operation_id: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CanvasAuthoringSnapshotV1 {
    pub revision: u64,
    pub scene: CanvasSceneV1,
    pub diagnostics: Vec<CanvasDiagnosticV1>,
}

/// A validated in-memory candidate, never proof that a drawing was published.
#[derive(Debug, Clone, PartialEq)]
pub struct CanvasAuthoringCandidate {
    pub session_id: String,
    pub title: String,
    pub scene: CanvasSceneV1,
    pub revision: u64,
    pub diagnostics: Vec<CanvasDiagnosticV1>,
    pub source: CanvasAuthoringSource,
    pub target: CanvasCompanionTargetSnapshot,
}

/// Created only by host acceptance. It has no serde authority/approval fields,
/// no filesystem access, no database handle and no ordinary Chat dispatch hook.
#[derive(Debug)]
pub struct CanvasAuthoringSession {
    context: CanvasAuthoringContext,
    source: CanvasAuthoringSource,
    target: CanvasCompanionTargetSnapshot,
    title: String,
    accepted_at: u64,
    expires_at: u64,
    state: CanvasAuthoringState,
    scene: CanvasSceneV1,
    revision: u64,
    receipts: BTreeMap<String, (String, CanvasAuthoringBatchReceiptV1)>,
    completion: Option<(String, u64, CanvasAuthoringCandidate)>,
}

impl CanvasAuthoringSession {
    #[allow(clippy::too_many_arguments)]
    pub fn from_host_acceptance(
        session_id: &str,
        source: CanvasAuthoringSource,
        target: CanvasCompanionTargetSnapshot,
        profile: CanvasProfileV1,
        title: &str,
        now: u64,
        expires_at: u64,
    ) -> Result<Self, Error> {
        if !valid_id(session_id)
            || expires_at <= now
            || expires_at
                .checked_sub(now)
                .is_none_or(|duration| duration > MAX_AUTHORING_SESSION_SECONDS)
        {
            return Err(invalid(
                "Canvas authoring session identity or lifetime is invalid",
            ));
        }
        let scene = normalize_scene(&CanvasSceneV1 {
            document: Some(CanvasDocumentV1 {
                schema_version: 1,
                profile,
                language: source.language.clone(),
                narrative: String::new(),
                provenance: Some(CanvasProvenanceV1 {
                    source_id: source.final_message_id.clone(),
                    source_revision: None,
                    source_sha256: source.source_sha256.clone(),
                }),
                elements: vec![],
            }),
            ..Default::default()
        })?;
        Ok(Self {
            context: CanvasAuthoringContext {
                project_id: source.project_id.clone(),
                conversation_id: source.conversation_id.clone(),
                session_id: session_id.into(),
                origin: "companion_canvas".into(),
            },
            source,
            target,
            title: sanitize_title(title)?,
            accepted_at: now,
            expires_at,
            state: CanvasAuthoringState::Active,
            scene,
            revision: 0,
            receipts: BTreeMap::new(),
            completion: None,
        })
    }

    pub fn context(&self) -> &CanvasAuthoringContext {
        &self.context
    }
    pub fn source(&self) -> &CanvasAuthoringSource {
        &self.source
    }
    pub fn target(&self) -> &CanvasCompanionTargetSnapshot {
        &self.target
    }
    pub fn state(&self) -> CanvasAuthoringState {
        self.state
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn cancel(&mut self) {
        self.state = CanvasAuthoringState::Cancelled;
    }

    pub fn close(&mut self) {
        self.state = CanvasAuthoringState::Closed;
    }

    fn check_context(&self, context: &CanvasAuthoringContext) -> Result<(), Error> {
        if context != &self.context {
            return Err(invalid(
                "Canvas authoring context does not match its host grant",
            ));
        }
        Ok(())
    }

    fn check_active(&self, context: &CanvasAuthoringContext, now: u64) -> Result<(), Error> {
        self.check_context(context)?;
        if self.state != CanvasAuthoringState::Active
            || now < self.accepted_at
            || now >= self.expires_at
        {
            return Err(invalid("Canvas authoring session is inactive or expired"));
        }
        Ok(())
    }

    pub fn inspect(
        &self,
        context: &CanvasAuthoringContext,
        now: u64,
    ) -> Result<CanvasAuthoringSnapshotV1, Error> {
        self.check_active(context, now)?;
        Ok(CanvasAuthoringSnapshotV1 {
            revision: self.revision,
            scene: self.scene.clone(),
            diagnostics: validate_document(&self.scene)?,
        })
    }

    pub fn validate(
        &self,
        context: &CanvasAuthoringContext,
        now: u64,
    ) -> Result<Vec<CanvasDiagnosticV1>, Error> {
        self.check_active(context, now)?;
        validate_document(&self.scene)
    }

    pub fn apply(
        &mut self,
        context: &CanvasAuthoringContext,
        batch: &CanvasAuthoringBatchV1,
        now: u64,
    ) -> Result<CanvasAuthoringBatchReceiptV1, Error> {
        self.check_active(context, now)?;
        let hash = batch.validate_limits()?;
        if let Some((previous_hash, receipt)) = self.receipts.get(&batch.operation_id) {
            return if previous_hash == &hash {
                Ok(receipt.clone())
            } else {
                Err(invalid(
                    "Canvas operation identity was already used for different content",
                ))
            };
        }
        if batch.expected_revision != self.revision {
            return Err(invalid("Canvas staging revision conflict"));
        }
        if self.receipts.len() >= MAX_AUTHORING_REQUESTS {
            return Err(invalid(
                "Canvas session reached its admitted operation limit",
            ));
        }
        let candidate = apply_canvas_commands(&self.scene, &batch.commands)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("Canvas staging revision overflow"))?;
        let receipt = CanvasAuthoringBatchReceiptV1 {
            operation_id: batch.operation_id.clone(),
            revision,
        };
        self.scene = candidate;
        self.revision = revision;
        self.receipts
            .insert(batch.operation_id.clone(), (hash, receipt.clone()));
        Ok(receipt)
    }

    pub fn finish_candidate(
        &mut self,
        context: &CanvasAuthoringContext,
        operation_id: &str,
        expected_revision: u64,
        now: u64,
    ) -> Result<CanvasAuthoringCandidate, Error> {
        self.check_context(context)?;
        if !valid_id(operation_id) {
            return Err(invalid("Canvas completion operation identity is invalid"));
        }
        if let Some((previous_id, previous_revision, candidate)) = &self.completion {
            return if self.state == CanvasAuthoringState::Finished
                && now >= self.accepted_at
                && now < self.expires_at
                && previous_id == operation_id
                && *previous_revision == expected_revision
            {
                Ok(candidate.clone())
            } else {
                Err(invalid(
                    "Canvas completion was already finalized for another request",
                ))
            };
        }
        self.check_active(context, now)?;
        if expected_revision != self.revision {
            return Err(invalid("Canvas completion revision conflict"));
        }
        if self.receipts.contains_key(operation_id) {
            return Err(invalid(
                "Canvas completion identity was already used by a batch",
            ));
        }
        if self.scene.elements.is_empty() {
            return Err(invalid("Canvas authoring cannot finish an empty candidate"));
        }
        let scene = normalize_scene(&self.scene)?;
        let diagnostics = validate_document(&scene)?;
        let candidate = CanvasAuthoringCandidate {
            session_id: self.context.session_id.clone(),
            title: self.title.clone(),
            scene,
            revision: self.revision,
            diagnostics,
            source: self.source.clone(),
            target: self.target.clone(),
        };
        self.state = CanvasAuthoringState::Finished;
        self.completion = Some((operation_id.into(), expected_revision, candidate.clone()));
        Ok(candidate)
    }
}

#[cfg(test)]
#[path = "authoring_tests.rs"]
mod tests;
