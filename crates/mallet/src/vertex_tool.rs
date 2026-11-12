//! Hammer's vertex tool (Shift+V): drag the corners and edge midpoints of the selected
//! brushes, split faces between two corners, and merge corners dragged onto each other.
//!
//! The brushes change as they are dragged, like Hammer++, and each drag undoes as one step.
//! Shapes that would not be convex are drawn in red and never written to the map.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use eframe::egui::Pos2;
use glam::DVec3;
use mallet_vmf::edit::Edit;
use mallet_vmf::vertex::{BrushMesh, VertexError, WELD_DISTANCE};
use mallet_vmf::{format_vec3, IdAllocator, Solid, Vmf};

use crate::editor::{Editor, Tool};
use crate::scene::Owner;

/// Handles closer than this many pixels to the nearest one count as stacked on it.
const STACK_PIXELS: f32 = 1.5;

/// Undo groups for vertex drags, kept far from the hashes Face Edit uses.
static NEXT_GROUP: AtomicU64 = AtomicU64::new(0x7654_0000_0000_0000);

/// A vertex or an edge of a brush, by where its corners are, so it can be found again
/// after the brush is rebuilt.
#[derive(Clone, Debug, PartialEq)]
pub struct Picked {
    pub solid: u32,
    /// One corner for a vertex, two for an edge.
    pub corners: Vec<DVec3>,
}

impl Picked {
    /// Where the handle is drawn: the corner, or the middle of the edge.
    pub fn position(&self) -> DVec3 {
        self.corners.iter().copied().sum::<DVec3>() / self.corners.len() as f64
    }

    pub fn is_edge(&self) -> bool {
        self.corners.len() == 2
    }

    fn shifted(&self, offset: DVec3) -> Self {
        Self {
            solid: self.solid,
            corners: self.corners.iter().map(|corner| *corner + offset).collect(),
        }
    }
}

/// A handle to draw: a vertex or edge midpoint of one of the edited brushes.
#[derive(Clone, Debug)]
pub struct VertexHandle {
    pub picked: Picked,
    pub selected: bool,
}

/// One brush being edited: its mesh, and the brush the mesh was last written as.
#[derive(Clone, Debug)]
struct VertexBrush {
    id: u32,
    mesh: BrushMesh,
    built: Solid,
}

/// A drag of the selected handles.
#[derive(Clone, Debug)]
struct VertexDrag {
    /// The brushes as they were when the drag began, with the vertices that move.
    start: Vec<(VertexBrush, Vec<usize>)>,
    /// The selection when the drag began.
    picked: Vec<Picked>,
    offset: DVec3,
    group: u64,
    applied: bool,
}

/// What the vertex tool remembers between frames.
#[derive(Clone, Debug, Default)]
pub struct VertexState {
    brushes: Vec<VertexBrush>,
    selected: Vec<Picked>,
    drag: Option<VertexDrag>,
    synced: Option<(u64, u64)>,
    /// Faces of a dragged shape that was refused, drawn in red.
    pub invalid: Vec<Vec<DVec3>>,
    /// Whether to ask if vertices dragged onto each other should merge.
    pub merge_prompt: bool,
}

impl VertexState {
    /// Every vertex handle, then every edge handle, of the brushes being edited.
    pub fn handles(&self) -> Vec<VertexHandle> {
        let mut vertices = Vec::new();
        let mut edges = Vec::new();
        for brush in &self.brushes {
            for &point in &brush.mesh.points {
                vertices.push(self.handle(brush.id, vec![point]));
            }
            for [a, b] in brush.mesh.edges() {
                edges.push(self.handle(brush.id, vec![brush.mesh.points[a], brush.mesh.points[b]]));
            }
        }
        vertices.extend(edges);
        vertices
    }

    fn handle(&self, solid: u32, corners: Vec<DVec3>) -> VertexHandle {
        let picked = Picked { solid, corners };
        let selected = self.selected.iter().any(|chosen| same(chosen, &picked));
        VertexHandle { picked, selected }
    }

    /// The handles under a screen point, given how to project world points. With `stack`,
    /// every handle drawn on the nearest one is included, as the 2D views show corners
    /// lying behind each other at one spot; corners win over edge midpoints there.
    pub fn hits(&self, project: impl Fn(DVec3) -> Option<Pos2>, at: Pos2, reach: f32, stack: bool) -> Vec<Picked> {
        let mut found: Vec<(f32, Picked)> = self
            .handles()
            .into_iter()
            .filter_map(|handle| {
                let distance = project(handle.picked.position())?.distance(at);
                (distance <= reach).then_some((distance, handle.picked))
            })
            .collect();
        if found.iter().any(|(_, picked)| !picked.is_edge()) {
            found.retain(|(_, picked)| !picked.is_edge());
        }
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        let Some(nearest) = found.first().map(|(distance, _)| *distance) else {
            return Vec::new();
        };
        if !stack {
            found.truncate(1);
        }
        found
            .into_iter()
            .filter(|(distance, _)| *distance <= nearest + STACK_PIXELS)
            .map(|(_, picked)| picked)
            .collect()
    }
}

/// Whether two picks are the same vertex or edge.
fn same(a: &Picked, b: &Picked) -> bool {
    a.solid == b.solid
        && a.corners.len() == b.corners.len()
        && (a.corners.iter().zip(&b.corners).all(|(x, y)| near(*x, *y))
            || a.corners.iter().rev().zip(&b.corners).all(|(x, y)| near(*x, *y)))
}

fn near(a: DVec3, b: DVec3) -> bool {
    a.distance(b) < WELD_DISTANCE
}

/// The index of the mesh vertex at `point`.
fn vertex_at(mesh: &BrushMesh, point: DVec3) -> Option<usize> {
    mesh.points.iter().position(|vertex| near(*vertex, point))
}

fn find_solid(vmf: &Vmf, id: u32) -> Option<&Solid> {
    vmf.all_entities()
        .flat_map(|entity| entity.solids())
        .find(|solid| solid.id() == Some(id))
}

impl Editor {
    /// The brushes the vertex tool edits: the selected brushes, and those of selected
    /// brush entities.
    fn vertex_solid_ids(&self) -> Vec<u32> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        for owner in &self.selection {
            match *owner {
                Owner::Solid(id) => ids.push(id),
                Owner::Entity(id) => {
                    if let Some(entity) = document.vmf.all_entities().find(|entity| entity.id() == Some(id)) {
                        ids.extend(entity.solids().filter_map(Solid::id));
                    }
                }
            }
        }
        ids
    }

    /// Brings the vertex tool up to date with the map and the selection. Brushes that
    /// changed outside the tool, as by undo, are read again; split faces are kept only
    /// while their brush is unchanged.
    pub fn sync_vertex_tool(&mut self) {
        if self.tool != Tool::Vertex {
            if self.vertex.synced.is_some() || !self.vertex.brushes.is_empty() {
                self.vertex = Default::default();
            }
            return;
        }
        let key = (self.scene.geometry_generation, self.scene.selection_generation);
        if self.vertex.synced == Some(key) {
            return;
        }
        self.vertex.synced = Some(key);
        let ids = self.vertex_solid_ids();
        let Some(document) = &self.document else {
            self.vertex.brushes.clear();
            return;
        };
        let mut brushes = Vec::new();
        let mut refused = None;
        for id in ids {
            let Some(solid) = find_solid(&document.vmf, id) else {
                continue;
            };
            let kept = self
                .vertex
                .brushes
                .iter()
                .find(|brush| brush.id == id && brush.built == *solid)
                .cloned();
            match kept {
                Some(brush) => brushes.push(brush),
                None => match BrushMesh::from_solid(solid) {
                    Ok(mesh) => brushes.push(VertexBrush {
                        id,
                        mesh,
                        built: solid.clone(),
                    }),
                    Err(error) => refused = Some(error),
                },
            }
        }
        self.vertex.brushes = brushes;
        let brushes = &self.vertex.brushes;
        self.vertex.selected.retain(|picked| {
            brushes.iter().any(|brush| {
                brush.id == picked.solid
                    && picked
                        .corners
                        .iter()
                        .all(|corner| vertex_at(&brush.mesh, *corner).is_some())
            })
        });
        if let Some(error) = refused {
            self.message = format!("Some brushes were left out: {error}");
        }
    }

    /// Selects handles that were clicked. Ctrl adds or removes them; a plain click on a
    /// selected handle keeps the selection, so several can be dragged together.
    pub fn vertex_pick(&mut self, hits: Vec<Picked>, ctrl: bool) {
        let state = &mut self.vertex;
        if hits.is_empty() {
            if !ctrl {
                state.selected.clear();
            }
            return;
        }
        if ctrl {
            for hit in hits {
                match state.selected.iter().position(|chosen| same(chosen, &hit)) {
                    Some(index) => {
                        state.selected.remove(index);
                    }
                    None => state.selected.push(hit),
                }
            }
        } else if !hits
            .iter()
            .any(|hit| state.selected.iter().any(|chosen| same(chosen, hit)))
        {
            state.selected = hits;
        }
        self.message = self.vertex_selection_summary();
    }

    /// Selects the corners that `inside` accepts, as a marquee does. Ctrl adds them to
    /// the selection.
    pub fn vertex_select_where(&mut self, inside: impl Fn(DVec3) -> bool, ctrl: bool) {
        let picked: Vec<Picked> = self
            .vertex
            .handles()
            .into_iter()
            .filter(|handle| !handle.picked.is_edge() && inside(handle.picked.position()))
            .map(|handle| handle.picked)
            .collect();
        if !ctrl {
            self.vertex.selected.clear();
        }
        for pick in picked {
            if !self.vertex.selected.iter().any(|chosen| same(chosen, &pick)) {
                self.vertex.selected.push(pick);
            }
        }
        self.message = self.vertex_selection_summary();
    }

    /// Clears the selected vertices and edges. Returns whether any were selected.
    pub fn clear_vertex_selection(&mut self) -> bool {
        !std::mem::take(&mut self.vertex.selected).is_empty()
    }

    fn vertex_selection_summary(&self) -> String {
        let edges = self.vertex.selected.iter().filter(|picked| picked.is_edge()).count();
        let vertices = self.vertex.selected.len() - edges;
        format!("{vertices} vertices and {edges} edges selected")
    }

    /// Starts dragging the selected handles.
    pub fn begin_vertex_drag(&mut self) {
        self.sync_vertex_tool();
        let mut start = Vec::new();
        for brush in &self.vertex.brushes {
            let mut moving = BTreeSet::new();
            for picked in self.vertex.selected.iter().filter(|picked| picked.solid == brush.id) {
                moving.extend(
                    picked
                        .corners
                        .iter()
                        .filter_map(|corner| vertex_at(&brush.mesh, *corner)),
                );
            }
            if !moving.is_empty() {
                start.push((brush.clone(), moving.into_iter().collect()));
            }
        }
        if start.is_empty() {
            return;
        }
        self.vertex.drag = Some(VertexDrag {
            start,
            picked: self.vertex.selected.clone(),
            offset: DVec3::ZERO,
            group: NEXT_GROUP.fetch_add(1, Ordering::Relaxed),
            applied: false,
        });
    }

    /// Moves the dragged handles `offset` away from where the drag began, rebuilding the
    /// brushes as they go. A shape that is not convex is shown in red instead, and the
    /// brushes keep their last good shape.
    pub fn drag_vertices(&mut self, offset: DVec3) {
        let Some(drag) = &self.vertex.drag else {
            return;
        };
        if drag.offset == offset {
            return;
        }
        let Some(document) = &self.document else {
            return;
        };
        let mut ids = IdAllocator::for_map(&document.vmf);
        let mut built = Vec::new();
        let mut refused = Vec::new();
        let mut error = None;
        for (brush, moving) in &drag.start {
            let moves: Vec<(usize, DVec3)> = moving
                .iter()
                .map(|&vertex| (vertex, brush.mesh.points[vertex] + offset))
                .collect();
            let moved = brush.mesh.moved(&moves);
            match moved.build(&brush.built, &mut ids) {
                Ok((solid, mesh)) => built.push(VertexBrush {
                    id: brush.id,
                    mesh,
                    built: solid,
                }),
                Err(problem) => {
                    error = Some(problem);
                    refused.extend(
                        moved
                            .faces
                            .iter()
                            .map(|face| face.vertices.iter().map(|&vertex| moved.points[vertex]).collect()),
                    );
                }
            }
        }
        let group = drag.group;
        let picked: Vec<Picked> = drag.picked.iter().map(|picked| picked.shifted(offset)).collect();
        if let Some(error) = error {
            self.vertex.invalid = refused;
            self.message = format!("Invalid shape: {error}");
            return;
        }
        let edit = Edit::ReplaceSolids(built.iter().map(|brush| brush.built.clone()).collect());
        if let Err(error) = self.apply_vertex_edit(&edit, group) {
            self.message = error;
            return;
        }
        self.vertex.invalid.clear();
        for brush in built {
            if let Some(slot) = self.vertex.brushes.iter_mut().find(|existing| existing.id == brush.id) {
                *slot = brush;
            }
        }
        self.vertex.synced = Some((self.scene.geometry_generation, self.scene.selection_generation));
        self.vertex.selected = picked;
        if let Some(drag) = &mut self.vertex.drag {
            drag.offset = offset;
            drag.applied = true;
        }
        self.message = format!("Moved vertices by {}", format_vec3(offset));
    }

    fn apply_vertex_edit(&mut self, edit: &Edit, group: u64) -> Result<(), String> {
        let document = self.document.as_mut().ok_or("no map is open")?;
        document
            .history
            .apply_grouped(&mut document.vmf, edit, group)
            .map_err(|error| error.to_string())?;
        document.dirty = true;
        self.rebuild_scene();
        Ok(())
    }

    /// Ends a drag. When dragged corners landed on other corners, asks whether to merge
    /// them, as Hammer does.
    pub fn end_vertex_drag(&mut self) {
        let Some(drag) = self.vertex.drag.take() else {
            return;
        };
        if !self.vertex.invalid.is_empty() {
            self.vertex.invalid.clear();
            self.message = "That shape would not be convex, so the brush keeps its last valid shape".to_owned();
        }
        if !drag.applied {
            return;
        }
        let overlapping = drag.start.iter().any(|(brush, moving)| {
            let moves: Vec<(usize, DVec3)> = moving
                .iter()
                .map(|&vertex| (vertex, brush.mesh.points[vertex] + drag.offset))
                .collect();
            brush.mesh.moved(&moves).overlaps(moving)
        });
        self.vertex.merge_prompt = overlapping;
    }

    /// Whether the arrow keys move handles: the vertex tool is active and some are selected.
    pub fn nudges_vertices(&self) -> bool {
        self.tool == Tool::Vertex && !self.vertex.selected.is_empty()
    }

    /// Moves the selected handles by `offset` as one undo step, as the arrow keys do with
    /// Hammer's vertex tool. Nothing moves while a drag or the merge question is open.
    pub fn nudge_vertices(&mut self, offset: DVec3) {
        if self.vertex.drag.is_some() || self.vertex.merge_prompt {
            return;
        }
        self.begin_vertex_drag();
        self.drag_vertices(offset);
        self.end_vertex_drag();
    }

    /// Answers the merge question: merging keeps the drag, otherwise it is undone.
    pub fn answer_vertex_merge(&mut self, merge: bool) {
        self.vertex.merge_prompt = false;
        if merge {
            self.message = "Merged vertices".to_owned();
        } else if self.step_history(false).is_ok() {
            self.message = "Moved the vertices back".to_owned();
        }
    }

    /// Splits a face between the two selected corners, as Hammer's Split Face (Ctrl+F).
    /// The halves stay in one plane until a corner on the split is moved.
    pub fn split_vertex_face(&mut self) -> Result<(), String> {
        if self.tool != Tool::Vertex {
            return Err("split faces with the vertex tool (Shift+V)".to_owned());
        }
        self.sync_vertex_tool();
        let corners: Vec<&Picked> = self.vertex.selected.iter().filter(|picked| !picked.is_edge()).collect();
        let [a, b] = corners.as_slice() else {
            return Err("select two vertices of one face to split it".to_owned());
        };
        if a.solid != b.solid {
            return Err(VertexError::NotOnOneFace.to_string());
        }
        let brush = self
            .vertex
            .brushes
            .iter_mut()
            .find(|brush| brush.id == a.solid)
            .ok_or("that brush is no longer selected")?;
        let (Some(first), Some(second)) = (
            vertex_at(&brush.mesh, a.corners[0]),
            vertex_at(&brush.mesh, b.corners[0]),
        ) else {
            return Err("the selected vertices are no longer on the brush".to_owned());
        };
        brush.mesh = brush
            .mesh
            .split_face(first, second)
            .map_err(|error| error.to_string())?;
        self.message = "Split the face; move a vertex on the split to bend it".to_owned();
        Ok(())
    }
}

#[cfg(test)]
mod nudge_tests {
    use super::*;
    use crate::editor::SelectMode;

    /// A 128 unit cube with the vertex tool on it and its four top corners selected.
    fn editor_with_top_selected() -> Editor {
        let mut editor = Editor::default();
        editor.new_map();
        let block = editor.create_block(DVec3::splat(-64.0), DVec3::splat(64.0)).unwrap();
        editor.select([Owner::Solid(block)], SelectMode::Replace);
        editor.set_tool(Tool::Vertex);
        editor.sync_vertex_tool();
        editor.vertex_select_where(|point| point.z > 0.0, false);
        editor
    }

    fn highest(editor: &Editor) -> f64 {
        editor
            .vertex
            .handles()
            .iter()
            .map(|handle| handle.picked.position().z)
            .fold(f64::MIN, f64::max)
    }

    #[test]
    fn a_nudge_moves_the_selected_corners_and_undoes_as_one_step() {
        let mut editor = editor_with_top_selected();
        assert!(editor.nudges_vertices());
        editor.nudge_vertices(DVec3::new(0.0, 0.0, 16.0));
        assert_eq!(highest(&editor), 80.0);
        assert_eq!(editor.vertex.selected.len(), 4, "the moved corners stay selected");
        editor.nudge_vertices(DVec3::new(0.0, 0.0, 16.0));
        assert_eq!(highest(&editor), 96.0);

        assert!(editor.step_history(false).unwrap());
        editor.sync_vertex_tool();
        assert_eq!(highest(&editor), 80.0);
    }

    #[test]
    fn nothing_is_nudged_without_selected_corners() {
        let mut editor = editor_with_top_selected();
        editor.clear_vertex_selection();
        assert!(!editor.nudges_vertices());
        editor.nudge_vertices(DVec3::new(0.0, 0.0, 16.0));
        assert_eq!(highest(&editor), 64.0);
    }

    #[test]
    fn a_nudge_into_a_flat_brush_is_refused() {
        let mut editor = editor_with_top_selected();
        editor.nudge_vertices(DVec3::new(0.0, 0.0, -128.0));
        assert_eq!(highest(&editor), 64.0);
        assert!(editor.vertex.invalid.is_empty());
    }
}
