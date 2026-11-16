//! Pure editor state. A complete pointer gesture is one history entry.
use std::collections::VecDeque;

#[path = "semantic_editing.rs"]
pub(crate) mod semantic_editing;
#[cfg(test)]
#[path = "semantic_editor_tests.rs"]
mod semantic_editor_tests;
use ikore_core::companion_drawings::semantics::{CanvasProfileV1, CanvasSemanticKindV1};

use ikore_core::companion_drawings::{
    CanvasConnectorCurve, CanvasElementKindV1 as Kind, CanvasElementV1 as Element,
    CanvasSceneV1 as Scene, CanvasSemanticColor as Color, CanvasStrokeRendering,
    CanvasStrokeStyle as Stroke, CanvasStrokeWidth as Width, CanvasViewportV1 as Viewport,
    geometry,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Tool {
    #[default]
    Select,
    Hand,
    Rectangle,
    Diamond,
    Ellipse,
    Arrow,
    Line,
    Text,
    Eraser,
}

impl Tool {
    pub const ALL: [(Self, &'static str); 9] = [
        (Self::Select, "Select · V"),
        (Self::Hand, "Pan · H"),
        (Self::Rectangle, "Box · R"),
        (Self::Diamond, "Decision · D"),
        (Self::Ellipse, "Ellipse · O"),
        (Self::Arrow, "Arrow · A"),
        (Self::Line, "Line · L"),
        (Self::Text, "Text · T"),
        (Self::Eraser, "Erase · E"),
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Hand => "Pan",
            Self::Rectangle => "Box",
            Self::Diamond => "Decision",
            Self::Ellipse => "Ellipse",
            Self::Arrow => "Arrow",
            Self::Line => "Line",
            Self::Text => "Text",
            Self::Eraser => "Erase",
        }
    }

    pub const fn shortcut(self) -> &'static str {
        match self {
            Self::Select => "V",
            Self::Hand => "H",
            Self::Rectangle => "R",
            Self::Diamond => "D",
            Self::Ellipse => "O",
            Self::Arrow => "A",
            Self::Line => "L",
            Self::Text => "T",
            Self::Eraser => "E",
        }
    }

    pub const fn icon(self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self {
            Self::Select => Icon::Cursor,
            Self::Hand => Icon::Hand,
            Self::Rectangle => Icon::Square,
            Self::Diamond => Icon::Diamond,
            Self::Ellipse => Icon::Ellipse,
            Self::Arrow => Icon::ArrowRight,
            Self::Line => Icon::Line,
            Self::Text => Icon::Type,
            Self::Eraser => Icon::Eraser,
        }
    }
}

#[derive(Clone)]
pub(crate) struct DrawingStyle {
    pub color: Color,
    pub width: Width,
    pub stroke: Stroke,
    pub fill: bool,
    pub round: bool,
    pub bidirectional: bool,
    pub font_size: f32,
    pub rendering: CanvasStrokeRendering,
    pub curve: CanvasConnectorCurve,
}

impl Default for DrawingStyle {
    fn default() -> Self {
        Self {
            color: Color::default(),
            width: Width::default(),
            stroke: Stroke::default(),
            fill: false,
            round: true,
            bidirectional: false,
            font_size: 16.,
            rendering: CanvasStrokeRendering::Sketch,
            curve: CanvasConnectorCurve::Straight,
        }
    }
}

#[derive(Clone)]
enum Gesture {
    Create {
        before: Scene,
        start: [f32; 2],
        id: String,
    },
    Move {
        before: Scene,
        start: [f32; 2],
        original: Element,
    },
    Resize {
        before: Scene,
        original: Element,
        handle: usize,
    },
    Endpoint {
        before: Scene,
        original: Element,
        end: usize,
    },
    Erase {
        before: Scene,
    },
    Pan {
        start: [f32; 2],
        viewport: Viewport,
    },
}

pub(crate) struct EditorState {
    pub scene: Scene,
    pub viewport: Viewport,
    pub selected: Option<String>,
    pub tool: Tool,
    pub locked: bool,
    pub style: DrawingStyle,
    pub dirty: bool,
    pub change: u64,
    pub space_pan: bool,
    pub snap_target: Option<String>,
    pub creation_kind: CanvasSemanticKindV1,
    pub participant_id: Option<String>,
    pub lane_id: Option<String>,
    pub semantic_source: Option<String>,
    pub semantic_target: Option<String>,
    pub semantic_error: Option<String>,
    pub diagnostics: Vec<ikore_core::companion_drawings::semantics::CanvasDiagnosticV1>,
    gesture: Option<Gesture>,
    undo: VecDeque<Scene>,
    redo: Vec<Scene>,
    history_bytes: usize,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            scene: Scene::default(),
            viewport: Viewport::default(),
            selected: None,
            tool: Tool::Select,
            locked: false,
            style: DrawingStyle::default(),
            dirty: false,
            change: 0,
            space_pan: false,
            snap_target: None,
            creation_kind: CanvasSemanticKindV1::Component,
            participant_id: None,
            lane_id: None,
            semantic_source: None,
            semantic_target: None,
            semantic_error: None,
            diagnostics: Vec::new(),
            gesture: None,
            undo: VecDeque::new(),
            redo: Vec::new(),
            history_bytes: 0,
        }
    }
}

impl EditorState {
    pub fn load(&mut self, scene: Scene, viewport: Viewport) {
        let creation_kind = if scene
            .document
            .as_ref()
            .is_some_and(|d| d.profile == CanvasProfileV1::Bpmn)
        {
            CanvasSemanticKindV1::Task
        } else {
            CanvasSemanticKindV1::Component
        };
        *self = Self {
            scene,
            viewport,
            tool: self.tool,
            locked: self.locked,
            style: self.style.clone(),
            creation_kind,
            ..Self::default()
        };
        self.refresh_diagnostics();
    }

    pub fn refresh_diagnostics(&mut self) {
        // Derived from the committed scene, never from a pointer preview or model output.
        match ikore_core::companion_drawings::semantics::validate_document(&self.scene) {
            Ok(diagnostics) => self.diagnostics = diagnostics,
            Err(error) => {
                self.diagnostics.clear();
                self.semantic_error = Some(error.to_string());
            }
        }
    }

    pub fn selected_element(&self) -> Option<&Element> {
        let id = self.selected.as_ref()?;
        self.scene.elements.iter().find(|e| &e.id == id)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn gesturing(&self) -> bool {
        self.gesture.is_some()
    }

    fn bytes(scene: &Scene) -> usize {
        // Count heap storage as well as the enum/element array; avoid serializing on mouse-move.
        std::mem::size_of::<Scene>()
            + scene.document.as_ref().map_or(0, |document| {
                serde_json::to_vec(document).map_or(0, |bytes| bytes.len())
            })
            + scene
                .elements
                .iter()
                .map(|e| {
                    std::mem::size_of::<Element>()
                        + e.id.len()
                        + e.label.len()
                        + e.container_id.as_ref().map_or(0, String::len)
                        + match &e.shape {
                            Kind::Arrow {
                                start_binding,
                                end_binding,
                                ..
                            } => {
                                start_binding.as_ref().map_or(0, |b| b.element_id.len())
                                    + end_binding.as_ref().map_or(0, |b| b.element_id.len())
                            }
                            _ => 0,
                        }
                })
                .sum::<usize>()
    }

    fn trim_history(&mut self) {
        self.history_bytes = self
            .undo
            .iter()
            .chain(self.redo.iter())
            .map(Self::bytes)
            .sum();
        while self.undo.len() + self.redo.len() > 100 || self.history_bytes > 32 * 1024 * 1024 {
            let old = self.undo.pop_front().or_else(|| {
                if self.redo.is_empty() {
                    None
                } else {
                    Some(self.redo.remove(0))
                }
            });
            let Some(old) = old else { break };
            self.history_bytes = self.history_bytes.saturating_sub(Self::bytes(&old));
        }
    }

    pub fn commit(&mut self, before: Scene) -> bool {
        geometry::update_bindings(&mut self.scene);
        geometry::update_containers(&mut self.scene);
        if before.document.is_some() {
            match self.validate_semantic_edit(&before, &self.scene) {
                Ok(scene) => {
                    self.scene = scene;
                    self.semantic_error = None;
                }
                Err(error) => {
                    self.scene = before;
                    self.semantic_error = Some(error);
                    self.keep_selection();
                    return false;
                }
            }
        }
        if before == self.scene {
            return false;
        }
        self.undo.push_back(before);
        self.redo.clear();
        self.trim_history();
        self.dirty = true;
        self.change = self.change.saturating_add(1);
        self.refresh_diagnostics();
        true
    }

    pub fn undo(&mut self) -> bool {
        self.cancel();
        let Some(scene) = self.undo.pop_back() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.scene, scene));
        self.trim_history();
        self.dirty = true;
        self.change = self.change.saturating_add(1);
        self.keep_selection();
        self.refresh_diagnostics();
        true
    }

    pub fn redo(&mut self) -> bool {
        self.cancel();
        let Some(scene) = self.redo.pop() else {
            return false;
        };
        self.undo
            .push_back(std::mem::replace(&mut self.scene, scene));
        self.trim_history();
        self.dirty = true;
        self.change = self.change.saturating_add(1);
        self.keep_selection();
        self.refresh_diagnostics();
        true
    }

    fn keep_selection(&mut self) {
        if !self
            .scene
            .elements
            .iter()
            .any(|e| Some(&e.id) == self.selected.as_ref())
        {
            self.selected = None;
        }
    }

    pub fn mutate(&mut self, f: impl FnOnce(&mut Scene, &Option<String>)) -> bool {
        self.cancel();
        let before = self.scene.clone();
        f(&mut self.scene, &self.selected);
        self.commit(before)
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some(id) = self.selected.take() else {
            return false;
        };
        self.mutate(|scene, _| {
            geometry::detach_element(scene, &id);
            scene.elements.retain(|e| e.id != id);
        })
    }

    pub fn handles(element: &Element) -> Vec<[f32; 2]> {
        if let Some(points) = geometry::connector_world_points(element) {
            return points.to_vec();
        }
        if matches!(element.shape, Kind::Text { .. }) {
            return Vec::new();
        }
        let (x, y, w, h) = (element.x, element.y, element.width, element.height);
        vec![
            [x, y],
            [x + w / 2., y],
            [x + w, y],
            [x + w, y + h / 2.],
            [x + w, y + h],
            [x + w / 2., y + h],
            [x, y + h],
            [x, y + h / 2.],
        ]
    }

    pub fn press(&mut self, screen: [f32; 2], middle: bool) {
        let space_pan = self.space_pan;
        self.cancel();
        self.space_pan = space_pan;
        self.snap_target = None;
        let world = geometry::screen_to_world(&self.viewport, screen);
        let tolerance = 8. / self.viewport.zoom;
        if middle || self.space_pan || self.tool == Tool::Hand {
            self.gesture = Some(Gesture::Pan {
                start: screen,
                viewport: self.viewport,
            });
            return;
        }
        let before = self.scene.clone();
        self.semantic_error = None;
        if self.scene.document.is_some()
            && !matches!(self.tool, Tool::Select | Tool::Eraser | Tool::Hand)
            && self.tool != semantic_editing::semantic_tool(self.creation_kind)
        {
            self.semantic_error =
                Some("Choose an explicit element type in the semantic toolbar.".into());
            return;
        }
        match self.tool {
            Tool::Select => {
                if let Some(element) = self.selected_element().cloned()
                    && let Some(handle) = Self::handles(&element)
                        .iter()
                        .position(|p| distance(*p, world) <= tolerance)
                {
                    self.gesture = Some(if element.shape.is_connector() {
                        Gesture::Endpoint {
                            before,
                            original: element,
                            end: handle,
                        }
                    } else {
                        Gesture::Resize {
                            before,
                            original: element,
                            handle,
                        }
                    });
                    return;
                }
                self.selected = geometry::hit_test(&self.scene, world, tolerance);
                if let Some(original) = self.selected_element().cloned() {
                    self.gesture = Some(Gesture::Move {
                        before,
                        start: world,
                        original,
                    });
                }
            }
            Tool::Eraser => {
                self.erase(world);
                self.gesture = Some(Gesture::Erase { before });
            }
            Tool::Hand => {}
            tool => {
                if self.scene.elements.len() >= ikore_core::companion_drawings::MAX_ELEMENTS {
                    return;
                }
                let shape = match tool {
                    Tool::Rectangle => Kind::Rectangle {
                        round: self.style.round,
                    },
                    Tool::Diamond => Kind::Diamond,
                    Tool::Ellipse => Kind::Ellipse,
                    Tool::Arrow => Kind::Arrow {
                        points: [[0., 0.], [1., 1.]],
                        bidirectional: self.style.bidirectional,
                        start_binding: None,
                        end_binding: None,
                    },
                    Tool::Line => Kind::Line {
                        points: [[0., 0.], [1., 1.]],
                    },
                    _ => Kind::Text {
                        font_size: self.style.font_size,
                    },
                };
                let mut element = Element::new(shape, world[0], world[1], 1., 1.);
                if tool == Tool::Text {
                    element.width = 180.;
                    element.height = 24.;
                }
                element.stroke_color = self.style.color;
                element.label_font_size = Some(self.style.font_size);
                element.stroke_width = self.style.width;
                element.stroke_style = self.style.stroke;
                element.stroke_rendering = self.style.rendering;
                element.connector_curve = self.style.curve;
                element.fill_enabled = self.style.fill;
                element.z_index = self
                    .scene
                    .elements
                    .iter()
                    .map(|e| e.z_index)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                self.selected = Some(element.id.clone());
                self.gesture = Some(Gesture::Create {
                    before,
                    start: world,
                    id: element.id.clone(),
                });
                self.scene.elements.push(element);
            }
        }
    }

    fn erase(&mut self, world: [f32; 2]) {
        if let Some(id) = geometry::hit_test(&self.scene, world, 7. / self.viewport.zoom) {
            geometry::detach_element(&mut self.scene, &id);
            self.scene.elements.retain(|e| e.id != id);
            self.keep_selection();
        }
    }

    pub fn drag(&mut self, screen: [f32; 2]) {
        let world = geometry::screen_to_world(&self.viewport, screen);
        let Some(gesture) = self.gesture.clone() else {
            return;
        };
        match gesture {
            Gesture::Pan { start, viewport } => {
                self.viewport.pan_x = viewport.pan_x + screen[0] - start[0];
                self.viewport.pan_y = viewport.pan_y + screen[1] - start[1];
            }
            Gesture::Erase { .. } => self.erase(world),
            Gesture::Create { start, id, .. } => {
                if let Some(element) = self.scene.elements.iter_mut().find(|e| e.id == id) {
                    if element.shape.is_connector() {
                        geometry::set_connector_points(element, [start, world]);
                    } else if !matches!(element.shape, Kind::Text { .. }) {
                        element.x = start[0].min(world[0]);
                        element.y = start[1].min(world[1]);
                        element.width = (world[0] - start[0]).abs().max(1.);
                        element.height = (world[1] - start[1]).abs().max(1.);
                    }
                }
                self.preview_snap(world, &id);
            }
            Gesture::Move {
                before,
                start,
                original,
            } => {
                self.scene = before;
                if let Some(element) = self.scene.elements.iter_mut().find(|e| e.id == original.id)
                {
                    element.x = original.x + world[0] - start[0];
                    element.y = original.y + world[1] - start[1];
                    if let Kind::Arrow {
                        start_binding,
                        end_binding,
                        ..
                    } = &mut element.shape
                    {
                        *start_binding = None;
                        *end_binding = None;
                    }
                }
                geometry::update_bindings(&mut self.scene);
            }
            Gesture::Resize {
                before,
                original,
                handle,
            } => {
                self.scene = before;
                if let Some(element) = self.scene.elements.iter_mut().find(|e| e.id == original.id)
                {
                    let mut left = original.x;
                    let mut right = original.x + original.width;
                    let mut top = original.y;
                    let mut bottom = original.y + original.height;
                    if matches!(handle, 0 | 6 | 7) {
                        left = world[0];
                    }
                    if matches!(handle, 2..=4) {
                        right = world[0];
                    }
                    if matches!(handle, 0..=2) {
                        top = world[1];
                    }
                    if matches!(handle, 4..=6) {
                        bottom = world[1];
                    }
                    element.x = left.min(right);
                    element.y = top.min(bottom);
                    element.width = (right - left).abs().max(1.);
                    element.height = (bottom - top).abs().max(1.);
                }
                geometry::update_bindings(&mut self.scene);
            }
            Gesture::Endpoint {
                before,
                original,
                end,
            } => {
                self.scene = before;
                if let Some(mut points) = geometry::connector_world_points(&original) {
                    points[end] = world;
                    if let Some(element) =
                        self.scene.elements.iter_mut().find(|e| e.id == original.id)
                    {
                        geometry::set_connector_points(element, points);
                        if let Kind::Arrow {
                            start_binding,
                            end_binding,
                            ..
                        } = &mut element.shape
                        {
                            if end == 0 {
                                *start_binding = None;
                            } else {
                                *end_binding = None;
                            }
                        }
                    }
                }
                self.preview_snap(world, &original.id);
            }
        }
    }

    fn preview_snap(&mut self, point: [f32; 2], exclude: &str) {
        self.snap_target =
            geometry::snap_binding(&self.scene, point, 14. / self.viewport.zoom, Some(exclude))
                .map(|b| b.element_id);
    }

    fn bind(&mut self, id: &str, end: Option<usize>) {
        let Some(points) = self
            .scene
            .elements
            .iter()
            .find(|e| e.id == id)
            .and_then(geometry::connector_world_points)
        else {
            return;
        };
        let start =
            geometry::snap_binding(&self.scene, points[0], 14. / self.viewport.zoom, Some(id));
        let finish =
            geometry::snap_binding(&self.scene, points[1], 14. / self.viewport.zoom, Some(id));
        if let Some(element) = self.scene.elements.iter_mut().find(|e| e.id == id)
            && let Kind::Arrow {
                start_binding,
                end_binding,
                ..
            } = &mut element.shape
        {
            if end.is_none() || end == Some(0) {
                *start_binding = start;
            }
            if end.is_none() || end == Some(1) {
                *end_binding = finish;
            }
        }
    }

    /// Returns the new text's ID so the view can focus the native text editor.
    pub fn release(&mut self) -> (bool, Option<String>) {
        self.snap_target = None;
        let Some(gesture) = self.gesture.take() else {
            return (false, None);
        };
        let mut text = None;
        let before = match gesture {
            Gesture::Pan { .. } => return (false, None),
            Gesture::Create { before, id, .. } => {
                let Some(element) = self.scene.elements.iter().find(|e| e.id == id) else {
                    return (false, None);
                };
                if matches!(element.shape, Kind::Text { .. }) {
                    text = Some(id.clone());
                } else if (element.shape.is_connector()
                    && element.width.hypot(element.height) < 3. / self.viewport.zoom)
                    || (!element.shape.is_connector()
                        && (element.width < 3. / self.viewport.zoom
                            || element.height < 3. / self.viewport.zoom))
                {
                    self.scene = before;
                    self.selected = None;
                    return (false, None);
                }
                self.bind(&id, None);
                if !self.locked {
                    self.tool = Tool::Select;
                }
                before
            }
            Gesture::Endpoint {
                before,
                original,
                end,
            } => {
                self.bind(&original.id, Some(end));
                before
            }
            Gesture::Move { before, .. }
            | Gesture::Resize { before, .. }
            | Gesture::Erase { before } => before,
        };
        (self.commit(before), text)
    }

    pub fn cancel(&mut self) {
        if let Some(gesture) = self.gesture.take() {
            match gesture {
                Gesture::Create { before, .. }
                | Gesture::Move { before, .. }
                | Gesture::Resize { before, .. }
                | Gesture::Endpoint { before, .. }
                | Gesture::Erase { before } => self.scene = before,
                Gesture::Pan { .. } => {}
            }
        }
        self.space_pan = false;
        self.snap_target = None;
        self.keep_selection();
    }
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_gesture_is_one_undo_and_camera_is_independent() {
        let mut editor = EditorState {
            tool: Tool::Rectangle,
            ..EditorState::default()
        };
        editor.press([20., 30.], false);
        for p in [[40., 50.], [80., 90.], [130., 110.]] {
            editor.drag(p);
        }
        assert!(editor.release().0);
        assert_eq!(editor.scene.elements.len(), 1);
        editor.viewport.pan_x = 130.;
        assert!(editor.undo());
        assert!(editor.scene.elements.is_empty());
        assert!(!editor.undo());
        assert_eq!(editor.viewport.pan_x, 130.);
        assert!(editor.redo());
        assert_eq!(editor.scene.elements.len(), 1);
    }

    #[test]
    fn cancel_restores_preview_without_dirtying_or_adding_history() {
        let mut editor = EditorState {
            tool: Tool::Diamond,
            ..EditorState::default()
        };
        editor.press([20., 30.], false);
        editor.drag([130., 110.]);
        editor.cancel();
        assert!(editor.scene.elements.is_empty());
        assert!(!editor.dirty);
        assert!(!editor.can_undo());
    }

    #[test]
    fn new_edit_clears_redo_and_eraser_drag_is_one_command() {
        let mut editor = EditorState::default();
        for x in [0., 200.] {
            editor.tool = Tool::Rectangle;
            editor.press([x, 0.], false);
            editor.drag([x + 100., 100.]);
            editor.release();
        }
        editor.tool = Tool::Eraser;
        editor.press([20., 20.], false);
        editor.drag([220., 20.]);
        editor.release();
        assert!(editor.scene.elements.is_empty());
        assert!(editor.undo());
        assert_eq!(editor.scene.elements.len(), 2);
        editor.delete_selection();
        editor.mutate(|scene, _| scene.show_grid = false);
        assert!(!editor.can_redo());
    }

    #[test]
    fn snap_tolerance_is_constant_in_screen_pixels() {
        for zoom in [0.1, 4.] {
            let mut editor = EditorState {
                viewport: Viewport {
                    zoom,
                    ..Viewport::default()
                },
                ..EditorState::default()
            };
            editor.scene.elements.push(Element::new(
                Kind::Rectangle { round: false },
                0.,
                0.,
                100.,
                100.,
            ));
            editor.tool = Tool::Arrow;
            editor.press([100. * zoom + 10., 50. * zoom], false);
            editor.drag([300. * zoom + 30., 50. * zoom]);
            editor.release();
            let arrow = editor.scene.elements.last().unwrap();
            assert!(matches!(
                &arrow.shape,
                Kind::Arrow {
                    start_binding: Some(_),
                    ..
                }
            ));
        }
    }

    #[test]
    fn temporary_space_pan_keeps_content_and_history_unchanged() {
        let mut editor = EditorState {
            space_pan: true,
            tool: Tool::Rectangle,
            ..EditorState::default()
        };
        editor.press([10., 20.], false);
        editor.drag([50., 80.]);
        assert_eq!(editor.viewport.pan_x, 40.);
        assert_eq!(editor.viewport.pan_y, 60.);
        assert!(!editor.release().0);
        assert!(editor.scene.elements.is_empty());
        assert!(!editor.can_undo());
    }

    #[test]
    fn deleting_bound_target_detaches_arrow_and_undo_restores_it() {
        let mut editor = EditorState::default();
        let target = Element::new(Kind::Rectangle { round: false }, 0., 0., 100., 100.);
        let target_id = target.id.clone();
        editor.scene.elements.push(target);
        editor.tool = Tool::Arrow;
        editor.press([100., 50.], false);
        editor.drag([250., 50.]);
        editor.release();
        editor.selected = Some(target_id.clone());
        assert!(editor.delete_selection());
        assert_eq!(editor.scene.elements.len(), 1);
        assert!(matches!(
            &editor.scene.elements[0].shape,
            Kind::Arrow {
                start_binding: None,
                ..
            }
        ));
        assert!(editor.undo());
        assert_eq!(editor.scene.elements.len(), 2);
        assert!(editor.scene.elements.iter().any(|element|matches!(&element.shape,Kind::Arrow{start_binding:Some(binding),..} if binding.element_id==target_id)));
    }

    #[test]
    fn canvas_sketch_curve_creation_and_style_changes_are_single_undo_steps() {
        let mut editor = EditorState {
            tool: Tool::Arrow,
            ..EditorState::default()
        };
        editor.style.curve = CanvasConnectorCurve::CurveRight;
        editor.press([10., 20.], false);
        editor.drag([210., 20.]);
        assert!(editor.release().0);
        let element = editor.scene.elements.last().expect("created curve");
        assert_eq!(element.stroke_rendering, CanvasStrokeRendering::Sketch);
        assert_eq!(element.connector_curve, CanvasConnectorCurve::CurveRight);
        assert!(
            editor.mutate(
                |scene, _| scene.elements[0].connector_curve = CanvasConnectorCurve::CurveLeft
            )
        );
        assert!(editor.undo());
        assert_eq!(
            editor.scene.elements[0].connector_curve,
            CanvasConnectorCurve::CurveRight
        );
        assert!(editor.undo());
        assert!(editor.scene.elements.is_empty());
        assert!(editor.redo());
        assert_eq!(
            editor.scene.elements[0].stroke_rendering,
            CanvasStrokeRendering::Sketch
        );
    }
}
