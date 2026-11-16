//! Explicit architectural/process semantics independent of visual containment and GPUI.
//!
//! This is a restricted Ikore profile based on BPMN, not an execution engine or a
//! BPMN interchange/conformance implementation. Diagnostics are draft-completeness
//! warnings; prohibited connections and broken references are validation errors.
use super::{CanvasElementKindV1, CanvasElementV1, CanvasSceneV1, MAX_ELEMENTS, MAX_LABEL_CHARS};
use crate::persistence_service::PersistenceServiceError as Error;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const CANVAS_DOCUMENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_CANVAS_LANGUAGE_BYTES: usize = 35;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasProfileV1 {
    #[default]
    Architecture,
    Bpmn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasSemanticKindV1 {
    Component,
    Boundary,
    Relationship,
    Annotation,
    Task,
    UserTask,
    ServiceTask,
    StartEvent,
    EndEvent,
    ExclusiveGateway,
    ParallelGateway,
    Participant,
    Lane,
    SequenceFlow,
    MessageFlow,
    Association,
}

impl CanvasSemanticKindV1 {
    fn is_activity(self) -> bool {
        matches!(self, Self::Task | Self::UserTask | Self::ServiceTask)
    }

    fn is_flow_node(self) -> bool {
        self.is_activity()
            || matches!(
                self,
                Self::StartEvent | Self::EndEvent | Self::ExclusiveGateway | Self::ParallelGateway
            )
    }

    fn is_connector(self) -> bool {
        matches!(
            self,
            Self::Relationship | Self::SequenceFlow | Self::MessageFlow | Self::Association
        )
    }

    fn belongs_to(self, profile: CanvasProfileV1) -> bool {
        match profile {
            CanvasProfileV1::Architecture => matches!(
                self,
                Self::Component
                    | Self::Boundary
                    | Self::Relationship
                    | Self::Annotation
                    | Self::Association
            ),
            CanvasProfileV1::Bpmn => {
                !matches!(self, Self::Component | Self::Boundary | Self::Relationship)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasSemanticElementV1 {
    /// Stable identity of exactly one visual element, not a coordinate-derived ID.
    pub element_id: String,
    pub kind: CanvasSemanticKindV1,
    pub participant_id: Option<String>,
    pub lane_id: Option<String>,
    pub source_id: Option<String>,
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasProvenanceV1 {
    /// Opaque host-owned source snapshot identity. It is never a filesystem path.
    pub source_id: String,
    pub source_revision: Option<u64>,
    pub source_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasDocumentV1 {
    pub schema_version: u32,
    pub profile: CanvasProfileV1,
    /// Bounded language tag; prose and labels may contain Unicode in that language.
    pub language: String,
    pub narrative: String,
    pub provenance: Option<CanvasProvenanceV1>,
    pub elements: Vec<CanvasSemanticElementV1>,
}

/// A completeness warning, never an authorization or a proof of process correctness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasDiagnosticV1 {
    pub code: String,
    pub element_id: Option<String>,
    pub message: String,
}

fn invalid(code: &str, message: &str, element_id: Option<&str>) -> Error {
    let identity = element_id
        .filter(|id| valid_id(id))
        .map_or(String::new(), |id| format!(" (element '{id}')"));
    Error::Validation(format!("{code}: {message}{identity}"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_'))
        // IDs stay opaque: never replace one with a redaction marker, which would
        // change its identity. Reject recognized secret-shaped IDs before errors
        // or warning payloads can carry them outside this validation boundary.
        && crate::secret_redactor::redact_secrets(id).is_ok_and(|result| result.text == id)
}

fn valid_language(language: &str) -> bool {
    if language.len() > MAX_CANVAS_LANGUAGE_BYTES {
        return false;
    }
    let mut subtags = language.split('-');
    let primary = subtags.next().unwrap_or_default();
    (2..=8).contains(&primary.len())
        && primary.bytes().all(|value| value.is_ascii_alphabetic())
        && subtags.all(|part| {
            (1..=8).contains(&part.len()) && part.bytes().all(|value| value.is_ascii_alphanumeric())
        })
}

fn safe_text(text: &str) -> bool {
    text.chars().all(|value| {
        (!value.is_control() || matches!(value, '\n' | '\r' | '\t'))
            && value != '\u{fffe}'
            && value != '\u{ffff}'
    })
}

fn validate_metadata(document: &CanvasDocumentV1) -> Result<(), Error> {
    if document.schema_version != CANVAS_DOCUMENT_SCHEMA_VERSION {
        return Err(invalid(
            "semantic_schema",
            "Unsupported Canvas document schema version",
            None,
        ));
    }
    if !valid_language(&document.language) {
        return Err(invalid(
            "semantic_language",
            "Canvas language must be a bounded language tag",
            None,
        ));
    }
    if document.narrative.chars().count() > MAX_LABEL_CHARS || !safe_text(&document.narrative) {
        return Err(invalid(
            "semantic_narrative",
            "Canvas narrative exceeds its text limit or contains unsupported controls",
            None,
        ));
    }
    if document.elements.len() > MAX_ELEMENTS {
        return Err(invalid(
            "semantic_limit",
            "A Canvas document supports at most 1000 semantic entries",
            None,
        ));
    }
    if let Some(provenance) = &document.provenance
        && (!valid_id(&provenance.source_id)
            || provenance.source_sha256.len() != 64
            || !provenance
                .source_sha256
                .bytes()
                .all(|value| value.is_ascii_hexdigit()))
    {
        return Err(invalid(
            "semantic_source",
            "Canvas provenance requires an opaque source ID and SHA-256 fingerprint",
            None,
        ));
    }
    Ok(())
}

fn shape_matches(kind: CanvasSemanticKindV1, shape: &CanvasElementKindV1) -> bool {
    use CanvasElementKindV1 as Shape;
    use CanvasSemanticKindV1 as Kind;
    match kind {
        Kind::Component => matches!(
            shape,
            Shape::Rectangle { .. } | Shape::Diamond | Shape::Ellipse
        ),
        Kind::Boundary
        | Kind::Task
        | Kind::UserTask
        | Kind::ServiceTask
        | Kind::Participant
        | Kind::Lane => matches!(shape, Shape::Rectangle { .. }),
        Kind::StartEvent | Kind::EndEvent => matches!(shape, Shape::Ellipse),
        Kind::ExclusiveGateway | Kind::ParallelGateway => matches!(shape, Shape::Diamond),
        Kind::Relationship => matches!(shape, Shape::Arrow { .. } | Shape::Line { .. }),
        Kind::SequenceFlow | Kind::MessageFlow => matches!(shape, Shape::Arrow { .. }),
        Kind::Association => matches!(shape, Shape::Line { .. }),
        Kind::Annotation => matches!(shape, Shape::Text { .. }),
    }
}

type SemanticMap<'a> = HashMap<&'a str, &'a CanvasSemanticElementV1>;

fn validate_ownership(
    entry: &CanvasSemanticElementV1,
    profile: CanvasProfileV1,
    entries: &SemanticMap<'_>,
) -> Result<(), Error> {
    use CanvasSemanticKindV1 as Kind;
    let identity = Some(entry.element_id.as_str());
    // Participants are roots, lanes have a participant parent, and flow nodes
    // may additionally name a lane. This bounded hierarchy cannot form cycles.
    if profile == CanvasProfileV1::Architecture
        || entry.kind == Kind::Participant
        || entry.kind.is_connector()
    {
        if entry.participant_id.is_some() || entry.lane_id.is_some() {
            return Err(invalid(
                "semantic_ownership",
                "This element cannot declare participant or lane ownership",
                identity,
            ));
        }
        return Ok(());
    }
    let needs_participant = entry.kind.is_flow_node() || entry.kind == Kind::Lane;
    let participant = match entry.participant_id.as_deref() {
        Some(id) => {
            let parent = entries
                .get(id)
                .filter(|parent| parent.kind == Kind::Participant)
                .ok_or_else(|| {
                    invalid(
                        "semantic_participant",
                        "Participant reference must name a participant root",
                        identity,
                    )
                })?;
            Some(parent.element_id.as_str())
        }
        None if needs_participant => {
            return Err(invalid(
                "semantic_participant",
                "A process node or lane requires an explicit participant",
                identity,
            ));
        }
        None => None,
    };
    if let Some(id) = entry.lane_id.as_deref() {
        if entry.kind == Kind::Lane {
            return Err(invalid(
                "semantic_lane",
                "Nested lane ownership is outside the supported profile",
                identity,
            ));
        }
        let lane = entries
            .get(id)
            .filter(|lane| lane.kind == Kind::Lane)
            .ok_or_else(|| {
                invalid(
                    "semantic_lane",
                    "Lane reference must name a supported lane",
                    identity,
                )
            })?;
        if participant.is_none() || lane.participant_id.as_deref() != participant {
            return Err(invalid(
                "semantic_lane",
                "Lane and member must belong to the same participant",
                identity,
            ));
        }
    }
    Ok(())
}

fn participant_of(entry: &CanvasSemanticElementV1) -> Option<&str> {
    if entry.kind == CanvasSemanticKindV1::Participant {
        Some(&entry.element_id)
    } else {
        entry.participant_id.as_deref()
    }
}

fn validate_connection(
    entry: &CanvasSemanticElementV1,
    element: &CanvasElementV1,
    entries: &SemanticMap<'_>,
) -> Result<(), Error> {
    use CanvasSemanticKindV1 as Kind;
    let identity = Some(entry.element_id.as_str());
    if !entry.kind.is_connector() {
        if entry.source_id.is_some() || entry.target_id.is_some() {
            return Err(invalid(
                "semantic_endpoints",
                "Only relationships can declare source and target IDs",
                identity,
            ));
        }
        return Ok(());
    }
    let source = entry
        .source_id
        .as_deref()
        .and_then(|id| entries.get(id))
        .copied()
        .ok_or_else(|| {
            invalid(
                "semantic_endpoints",
                "Connection source must name an existing semantic element",
                identity,
            )
        })?;
    let target = entry
        .target_id
        .as_deref()
        .and_then(|id| entries.get(id))
        .copied()
        .ok_or_else(|| {
            invalid(
                "semantic_endpoints",
                "Connection target must name an existing semantic element",
                identity,
            )
        })?;
    match entry.kind {
        Kind::Relationship => {
            if ![source.kind, target.kind]
                .iter()
                .all(|kind| matches!(kind, Kind::Component | Kind::Boundary))
            {
                return Err(invalid(
                    "architecture_relationship",
                    "Architecture relationships connect components or boundaries",
                    identity,
                ));
            }
        }
        Kind::SequenceFlow => {
            if !source.kind.is_flow_node()
                || !target.kind.is_flow_node()
                || source.participant_id.is_none()
                || source.participant_id != target.participant_id
            {
                return Err(invalid(
                    "bpmn_sequence_scope",
                    "BPMN sequence flow connects flow nodes within one participant/process",
                    identity,
                ));
            }
            if target.kind == Kind::StartEvent || source.kind == Kind::EndEvent {
                return Err(invalid(
                    "bpmn_event_direction",
                    "BPMN sequence flow cannot enter a start event or leave an end event",
                    identity,
                ));
            }
        }
        Kind::MessageFlow => {
            if ![source.kind, target.kind]
                .iter()
                .all(|kind| kind.is_activity() || *kind == Kind::Participant)
                || participant_of(source).is_none()
                || participant_of(source) == participant_of(target)
            {
                return Err(invalid(
                    "bpmn_message_scope",
                    "BPMN message flow requires activities or participants in different pools",
                    identity,
                ));
            }
        }
        Kind::Association => {
            let one_annotation =
                (source.kind == Kind::Annotation) != (target.kind == Kind::Annotation);
            if !one_annotation || source.kind.is_connector() || target.kind.is_connector() {
                return Err(invalid(
                    "semantic_association",
                    "An association connects one annotation to a non-connector element",
                    identity,
                ));
            }
        }
        _ => unreachable!("non-connectors returned above"),
    }
    if let CanvasElementKindV1::Arrow {
        start_binding,
        end_binding,
        bidirectional,
        ..
    } = &element.shape
    {
        if start_binding
            .as_ref()
            .map(|binding| binding.element_id.as_str())
            != Some(source.element_id.as_str())
            || end_binding
                .as_ref()
                .map(|binding| binding.element_id.as_str())
                != Some(target.element_id.as_str())
        {
            return Err(invalid(
                "semantic_binding",
                "Arrow bindings must agree with explicit semantic endpoints",
                identity,
            ));
        }
        if *bidirectional && matches!(entry.kind, Kind::SequenceFlow | Kind::MessageFlow) {
            return Err(invalid(
                "bpmn_direction",
                "BPMN sequence and message flows have one explicit direction",
                identity,
            ));
        }
    }
    Ok(())
}

fn warning(code: &str, message: &str, element_id: Option<&str>) -> CanvasDiagnosticV1 {
    CanvasDiagnosticV1 {
        code: code.into(),
        element_id: element_id.map(str::to_owned),
        message: message.into(),
    }
}

fn completeness(
    document: &CanvasDocumentV1,
    visuals: &HashMap<&str, &CanvasElementV1>,
) -> Vec<CanvasDiagnosticV1> {
    use CanvasSemanticKindV1 as Kind;
    let mut diagnostics = Vec::new();
    if document.elements.is_empty() {
        diagnostics.push(warning(
            "empty_document",
            "The document has no semantic elements yet",
            None,
        ));
    }
    for entry in &document.elements {
        if visuals[entry.element_id.as_str()].label.trim().is_empty() {
            diagnostics.push(warning(
                "missing_label",
                "This element has no explanatory label",
                Some(&entry.element_id),
            ));
        }
    }
    if document.profile == CanvasProfileV1::Bpmn {
        let mut degrees: HashMap<&str, (usize, usize)> = HashMap::new();
        for entry in &document.elements {
            if entry.kind == Kind::SequenceFlow {
                if let Some(source) = entry.source_id.as_deref() {
                    degrees.entry(source).or_default().1 += 1;
                }
                if let Some(target) = entry.target_id.as_deref() {
                    degrees.entry(target).or_default().0 += 1;
                }
            }
        }
        for pool in document
            .elements
            .iter()
            .filter(|entry| entry.kind == Kind::Participant)
        {
            let nodes: Vec<_> = document
                .elements
                .iter()
                .filter(|entry| {
                    entry.kind.is_flow_node()
                        && entry.participant_id.as_deref() == Some(pool.element_id.as_str())
                })
                .collect();
            // A participant without a process is a valid black-box participant.
            if !nodes.is_empty() {
                if !nodes.iter().any(|entry| entry.kind == Kind::StartEvent) {
                    diagnostics.push(warning(
                        "bpmn_missing_start",
                        "The depicted process has no start event",
                        Some(&pool.element_id),
                    ));
                }
                if !nodes.iter().any(|entry| entry.kind == Kind::EndEvent) {
                    diagnostics.push(warning(
                        "bpmn_missing_end",
                        "The depicted process has no end event",
                        Some(&pool.element_id),
                    ));
                }
            }
        }
        for entry in document
            .elements
            .iter()
            .filter(|entry| entry.kind.is_flow_node())
        {
            let (incoming, outgoing) = degrees
                .get(entry.element_id.as_str())
                .copied()
                .unwrap_or_default();
            if incoming == 0 && entry.kind != Kind::StartEvent {
                diagnostics.push(warning(
                    "bpmn_no_incoming",
                    "This process node has no incoming sequence flow",
                    Some(&entry.element_id),
                ));
            }
            if outgoing == 0 && entry.kind != Kind::EndEvent {
                diagnostics.push(warning(
                    "bpmn_no_outgoing",
                    "This process node has no outgoing sequence flow",
                    Some(&entry.element_id),
                ));
            }
            if matches!(entry.kind, Kind::ExclusiveGateway | Kind::ParallelGateway)
                && incoming < 2
                && outgoing < 2
            {
                diagnostics.push(warning(
                    "bpmn_incomplete_gateway",
                    "The gateway does not yet depict a split or join",
                    Some(&entry.element_id),
                ));
            }
        }
    }
    diagnostics.sort_by(|left, right| {
        (&left.element_id, &left.code).cmp(&(&right.element_id, &right.code))
    });
    diagnostics
}

/// Validate explicit semantics without inferring roles from geometry or changing a draft.
/// Geometry itself is validated by the surrounding `normalize_scene` boundary.
pub fn validate_document(scene: &CanvasSceneV1) -> Result<Vec<CanvasDiagnosticV1>, Error> {
    let Some(document) = &scene.document else {
        return Ok(Vec::new());
    };
    validate_metadata(document)?;
    if scene.elements.len() != document.elements.len() || scene.elements.len() > MAX_ELEMENTS {
        return Err(invalid(
            "semantic_coverage",
            "Every visual element requires exactly one semantic entry",
            None,
        ));
    }
    let mut visuals = HashMap::new();
    for element in &scene.elements {
        if !valid_id(&element.id) || visuals.insert(element.id.as_str(), element).is_some() {
            return Err(invalid(
                "semantic_visual_id",
                "Visual element IDs must be valid and unique",
                None,
            ));
        }
    }
    let mut entries = SemanticMap::new();
    for entry in &document.elements {
        if !valid_id(&entry.element_id)
            || entries.insert(entry.element_id.as_str(), entry).is_some()
        {
            return Err(invalid(
                "semantic_id",
                "Semantic element IDs must be valid and unique",
                None,
            ));
        }
        if [
            entry.participant_id.as_deref(),
            entry.lane_id.as_deref(),
            entry.source_id.as_deref(),
            entry.target_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|id| !valid_id(id))
        {
            return Err(invalid(
                "semantic_reference_id",
                "Semantic references require bounded opaque IDs",
                Some(&entry.element_id),
            ));
        }
        let visual = visuals.get(entry.element_id.as_str()).ok_or_else(|| {
            invalid(
                "semantic_visual",
                "Semantic entry does not name a visual element",
                Some(&entry.element_id),
            )
        })?;
        if !entry.kind.belongs_to(document.profile) {
            return Err(invalid(
                "semantic_profile",
                "Element kind is outside the selected Canvas profile",
                Some(&entry.element_id),
            ));
        }
        if !shape_matches(entry.kind, &visual.shape) {
            return Err(invalid(
                "semantic_shape",
                "Visual primitive does not match the explicit semantic kind",
                Some(&entry.element_id),
            ));
        }
    }
    for entry in &document.elements {
        validate_ownership(entry, document.profile, &entries)?;
        validate_connection(entry, visuals[entry.element_id.as_str()], &entries)?;
    }
    Ok(completeness(document, &visuals))
}

/// Sanitize and canonically order a valid semantic document, atomically in memory.
/// Failure leaves the supplied scene untouched and never reports raw source text.
pub fn normalize_document(scene: &mut CanvasSceneV1) -> Result<Vec<CanvasDiagnosticV1>, Error> {
    validate_document(scene)?;
    let Some(mut document) = scene.document.clone() else {
        return Ok(Vec::new());
    };
    document.narrative = crate::secret_redactor::redact_secrets(&document.narrative)
        .map_err(|_| {
            invalid(
                "semantic_redaction",
                "Canvas narrative could not be sanitized",
                None,
            )
        })?
        .text
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if let Some(provenance) = &mut document.provenance {
        provenance.source_id = crate::secret_redactor::redact_secrets(&provenance.source_id)
            .map_err(|_| {
                invalid(
                    "semantic_redaction",
                    "Canvas provenance could not be sanitized",
                    None,
                )
            })?
            .text;
        provenance.source_sha256.make_ascii_lowercase();
    }
    document
        .elements
        .sort_by(|left, right| left.element_id.cmp(&right.element_id));
    let mut candidate = scene.clone();
    candidate.document = Some(document);
    let diagnostics = validate_document(&candidate)?;
    scene.document = candidate.document;
    Ok(diagnostics)
}
