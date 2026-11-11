//! Manual commands share the staged author's validation boundary and scene history.
use super::{EditorState, Element, Kind, Scene, Tool};
use ikore_core::companion_drawings::{
    ArrowBindingV1,
    authoring::{CanvasCommandV1, apply_canvas_commands},
    semantics::{CanvasProfileV1, CanvasSemanticElementV1, CanvasSemanticKindV1 as Semantic},
};

pub(crate) fn semantic_kinds(profile: CanvasProfileV1) -> &'static [Semantic] {
    use Semantic::*;
    match profile {
        CanvasProfileV1::Architecture => {
            &[Component, Boundary, Relationship, Annotation, Association]
        }
        CanvasProfileV1::Bpmn => &[
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
            Annotation,
            Association,
        ],
    }
}

pub(crate) fn semantic_name(kind: Semantic) -> &'static str {
    use Semantic::*;
    match kind {
        Component => "Component",
        Boundary => "Boundary",
        Relationship => "Relationship",
        Annotation => "Annotation",
        Task => "Task",
        UserTask => "User task",
        ServiceTask => "Service task",
        StartEvent => "None start event",
        EndEvent => "None end event",
        ExclusiveGateway => "Exclusive gateway",
        ParallelGateway => "Parallel gateway",
        Participant => "Participant / pool",
        Lane => "Lane",
        SequenceFlow => "Sequence flow",
        MessageFlow => "Message flow",
        Association => "Association",
    }
}

pub(crate) fn semantic_tool(kind: Semantic) -> Tool {
    use Semantic::*;
    match kind {
        Component | Boundary | Task | UserTask | ServiceTask | Participant | Lane => {
            Tool::Rectangle
        }
        StartEvent | EndEvent => Tool::Ellipse,
        ExclusiveGateway | ParallelGateway => Tool::Diamond,
        Relationship | SequenceFlow | MessageFlow => Tool::Arrow,
        Annotation => Tool::Text,
        Association => Tool::Line,
    }
}

fn connector(kind: Semantic) -> bool {
    matches!(
        kind,
        Semantic::Relationship
            | Semantic::SequenceFlow
            | Semantic::MessageFlow
            | Semantic::Association
    )
}

fn binding(id: &str, source: bool) -> ArrowBindingV1 {
    ArrowBindingV1 {
        element_id: id.into(),
        anchor: if source { [1., 0.] } else { [-1., 0.] },
        gap: 0.,
    }
}

fn set_bindings(element: &mut Element, semantic: &CanvasSemanticElementV1) {
    if let Kind::Arrow {
        start_binding,
        end_binding,
        ..
    } = &mut element.shape
    {
        if start_binding.as_ref().map(|b| b.element_id.as_str()) != semantic.source_id.as_deref() {
            *start_binding = semantic.source_id.as_deref().map(|id| binding(id, true));
        }
        if end_binding.as_ref().map(|b| b.element_id.as_str()) != semantic.target_id.as_deref() {
            *end_binding = semantic.target_id.as_deref().map(|id| binding(id, false));
        }
    }
}

impl EditorState {
    pub fn selected_semantic(&self) -> Option<&CanvasSemanticElementV1> {
        let id = self.selected.as_deref()?;
        self.scene
            .document
            .as_ref()?
            .elements
            .iter()
            .find(|e| e.element_id == id)
    }

    pub fn set_creation_kind(&mut self, kind: Semantic) {
        self.cancel();
        if self
            .scene
            .document
            .as_ref()
            .is_some_and(|d| semantic_kinds(d.profile).contains(&kind))
        {
            self.creation_kind = kind;
            self.selected = None;
            self.tool = semantic_tool(kind);
            self.semantic_error = None;
        }
    }

    fn execute_semantic(&mut self, commands: &[CanvasCommandV1]) -> bool {
        self.cancel();
        let before = self.scene.clone();
        match apply_canvas_commands(&before, commands) {
            Ok(scene) => {
                self.scene = scene;
                self.commit(before)
            }
            Err(error) => {
                self.semantic_error = Some(error.to_string());
                false
            }
        }
    }

    pub fn set_selected_kind(&mut self, kind: Semantic) -> bool {
        let Some(mut semantic) = self.selected_semantic().cloned() else {
            return false;
        };
        let Some(mut element) = self.selected_element().cloned() else {
            return false;
        };
        semantic.kind = kind;
        let old_connector = element.shape.is_connector();
        element.shape = match semantic_tool(kind) {
            Tool::Rectangle => Kind::Rectangle {
                round: matches!(
                    kind,
                    Semantic::Task | Semantic::UserTask | Semantic::ServiceTask
                ),
            },
            Tool::Ellipse => Kind::Ellipse,
            Tool::Diamond => Kind::Diamond,
            Tool::Text => Kind::Text { font_size: 16. },
            Tool::Arrow => Kind::Arrow {
                points: [[0., 0.], [element.width, element.height]],
                bidirectional: false,
                start_binding: None,
                end_binding: None,
            },
            Tool::Line => Kind::Line {
                points: [[0., 0.], [element.width, element.height]],
            },
            _ => return false,
        };
        if !connector(kind) {
            semantic.source_id = None;
            semantic.target_id = None;
        }
        if matches!(kind, Semantic::Participant | Semantic::Annotation) || connector(kind) {
            semantic.participant_id = None;
            semantic.lane_id = None;
        }
        if kind == Semantic::Lane {
            semantic.lane_id = None;
        }
        if connector(kind) && !old_connector {
            semantic.source_id = None;
            semantic.target_id = None;
        }
        set_bindings(&mut element, &semantic);
        self.execute_semantic(&[CanvasCommandV1::UpsertElement {
            element: Box::new(element),
            semantic: Some(semantic),
        }])
    }

    pub fn set_membership(
        &mut self,
        participant_id: Option<String>,
        lane_id: Option<String>,
    ) -> bool {
        let Some(mut semantic) = self.selected_semantic().cloned() else {
            self.participant_id = participant_id;
            self.lane_id = lane_id;
            return true;
        };
        let Some(element) = self.selected_element().cloned() else {
            return false;
        };
        semantic.participant_id = participant_id;
        semantic.lane_id = lane_id;
        self.execute_semantic(&[CanvasCommandV1::UpsertElement {
            element: Box::new(element),
            semantic: Some(semantic),
        }])
    }

    pub fn set_endpoint(&mut self, source: bool, id: &str) -> bool {
        let Some(mut semantic) = self.selected_semantic().cloned() else {
            if source {
                self.semantic_source = Some(id.into())
            } else {
                self.semantic_target = Some(id.into())
            }
            return true;
        };
        if !connector(semantic.kind) {
            return false;
        }
        let Some(mut element) = self.selected_element().cloned() else {
            return false;
        };
        if source {
            semantic.source_id = Some(id.into())
        } else {
            semantic.target_id = Some(id.into())
        }
        set_bindings(&mut element, &semantic);
        self.execute_semantic(&[CanvasCommandV1::UpsertElement {
            element: Box::new(element),
            semantic: Some(semantic),
        }])
    }

    /// Convert the completed gesture into one validated batch. No partial edit
    /// or inferred process membership can enter history or persistence.
    pub(super) fn validate_semantic_edit(
        &self,
        before: &Scene,
        preview: &Scene,
    ) -> Result<Scene, String> {
        let mut removals: Vec<_> = before
            .elements
            .iter()
            .filter(|e| !preview.elements.iter().any(|p| p.id == e.id))
            .collect();
        // The core removes a node's connections as part of node deletion. Drop
        // explicit connections first so one eraser gesture never removes them
        // twice; process leaves precede lanes and participant containers.
        removals.sort_by_key(|element| {
            if element.shape.is_connector() {
                return 0;
            }
            match before
                .document
                .as_ref()
                .and_then(|document| {
                    document
                        .elements
                        .iter()
                        .find(|entry| entry.element_id == element.id)
                })
                .map(|entry| entry.kind)
            {
                Some(Semantic::Participant) => 3,
                Some(Semantic::Lane) => 2,
                _ => 1,
            }
        });
        let mut commands: Vec<_> = removals
            .into_iter()
            .map(|e| CanvasCommandV1::RemoveElement {
                element_id: e.id.clone(),
            })
            .collect();
        let base = if commands.is_empty() {
            before.clone()
        } else {
            apply_canvas_commands(before, &commands).map_err(|e| e.to_string())?
        };
        for element in &preview.elements {
            let old = before.elements.iter().find(|e| e.id == element.id);
            // Removing a node also removes dependent connections in the core.
            if old.is_some() && !base.elements.iter().any(|e| e.id == element.id) {
                continue;
            }
            let mut element = element.clone();
            let existing = preview
                .document
                .as_ref()
                .and_then(|d| d.elements.iter().find(|s| s.element_id == element.id));
            let semantic = if let Some(semantic) = existing {
                semantic.clone()
            } else {
                let kind = self.creation_kind;
                let owned = matches!(
                    kind,
                    Semantic::Task
                        | Semantic::UserTask
                        | Semantic::ServiceTask
                        | Semantic::StartEvent
                        | Semantic::EndEvent
                        | Semantic::ExclusiveGateway
                        | Semantic::ParallelGateway
                        | Semantic::Lane
                );
                CanvasSemanticElementV1 {
                    element_id: element.id.clone(),
                    kind,
                    participant_id: if owned {
                        self.participant_id.clone()
                    } else {
                        None
                    },
                    lane_id: if owned && kind != Semantic::Lane {
                        self.lane_id.clone()
                    } else {
                        None
                    },
                    source_id: if connector(kind) {
                        self.semantic_source.clone()
                    } else {
                        None
                    },
                    target_id: if connector(kind) {
                        self.semantic_target.clone()
                    } else {
                        None
                    },
                }
            };
            let mut semantic = semantic;
            if let Kind::Arrow {
                start_binding,
                end_binding,
                ..
            } = &element.shape
            {
                if old.is_some() && (start_binding.is_none() || end_binding.is_none()) {
                    return Err(
                        "Semantic connections must remain attached to explicit endpoints.".into(),
                    );
                }
                // Dragging an endpoint explicitly requests a new connection.
                semantic.source_id = start_binding
                    .as_ref()
                    .map(|b| b.element_id.clone())
                    .or(semantic.source_id);
                semantic.target_id = end_binding
                    .as_ref()
                    .map(|b| b.element_id.clone())
                    .or(semantic.target_id);
                set_bindings(&mut element, &semantic);
            }
            if old != Some(&element)
                || before
                    .document
                    .as_ref()
                    .and_then(|d| d.elements.iter().find(|s| s.element_id == element.id))
                    != Some(&semantic)
            {
                commands.push(CanvasCommandV1::UpsertElement {
                    element: Box::new(element),
                    semantic: Some(semantic),
                });
            }
        }
        let mut candidate = if commands.is_empty() {
            before.clone()
        } else {
            apply_canvas_commands(before, &commands).map_err(|e| e.to_string())?
        };
        candidate.background_mode = preview.background_mode;
        candidate.show_grid = preview.show_grid;
        ikore_core::companion_drawings::normalize_scene(&candidate).map_err(|e| e.to_string())
    }
}
