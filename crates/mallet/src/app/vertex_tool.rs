//! The vertex tool in the viewports: picking and dragging corners and edge midpoints in
//! the 2D and 3D views, drawing their handles and refused shapes, and the question Hammer
//! asks when corners are dragged onto each other.

use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Response, Stroke};
use glam::{DVec2, DVec3};

use super::{pick, pick_tolerance, snap, MalletApp, PointerState};
use crate::canvas::Canvas;
use crate::editor::{Editor, Tool};
use crate::view2d::View2D;
use crate::view3d::Camera3D;

/// How close, in pixels, a press has to be to a handle to grab it.
const REACH_PIXELS: f32 = 6.0;
/// Half the side length, in pixels, of a corner handle.
const VERTEX_HALF_SIZE: f32 = 3.0;
/// Half the side length, in pixels, of an edge midpoint handle.
const EDGE_HALF_SIZE: f32 = 2.0;
const VERTEX_COLOR: Color32 = Color32::from_rgb(240, 240, 240);
const EDGE_COLOR: Color32 = Color32::from_rgb(240, 200, 60);
const SELECTED_COLOR: Color32 = Color32::from_rgb(255, 40, 40);
const INVALID_COLOR: Color32 = Color32::from_rgb(255, 60, 60);
const INVALID_FILL: Color32 = Color32::from_rgba_premultiplied(110, 0, 0, 110);

/// A left-button drag of the vertex tool.
#[derive(Clone, Copy, Debug)]
pub(super) enum VertexPointer {
    /// Moving the selected handles in a 2D view, from the grabbed handle at `anchor`.
    Move2D { view: usize, start: DVec2, anchor: DVec3 },
    /// Selecting the corners inside a rectangle in a 2D view.
    Marquee { view: usize, start: DVec2 },
    /// Moving the selected handles in a 3D view: across the floor, or up and down while
    /// Shift was held when the drag began.
    Move3D { slot: usize, anchor: DVec3, vertical: bool },
}

impl MalletApp {
    /// Runs the vertex tool for a click or drag in one 2D view.
    pub(super) fn vertex_tool_2d(
        &mut self,
        index: usize,
        view: &View2D,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) {
        let project = |point: DVec3| Some(view.to_screen(rect, view.axis.project(point)));
        if response.drag_started_by(PointerButton::Primary) {
            if let Some(origin) = pointer.press_origin {
                let start = view.to_world(rect, origin);
                let hits = editor.vertex.hits(project, origin, REACH_PIXELS, true);
                self.vertex_drag = Some(match hits.first().map(|hit| hit.position()) {
                    Some(anchor) => {
                        editor.vertex_pick(hits, pointer.modifiers.ctrl);
                        editor.begin_vertex_drag();
                        VertexPointer::Move2D {
                            view: index,
                            start,
                            anchor,
                        }
                    }
                    None => VertexPointer::Marquee { view: index, start },
                });
            }
        }
        let drag = match self.vertex_drag {
            Some(drag @ (VertexPointer::Move2D { view, .. } | VertexPointer::Marquee { view, .. }))
                if view == index =>
            {
                drag
            }
            _ => {
                if response.clicked_by(PointerButton::Primary) {
                    if let Some(position) = pointer.position {
                        self.vertex_click_2d(view, rect, position, editor, pointer);
                    }
                }
                return;
            }
        };
        if let Some(position) = pointer.position {
            let world = view.to_world(rect, position);
            match drag {
                VertexPointer::Move2D { start, anchor, .. } => {
                    let corner = view.axis.project(anchor);
                    let offset = view.axis.lift(snap(corner + world - start, editor.snap_step()) - corner);
                    editor.drag_vertices(offset);
                }
                VertexPointer::Marquee { start, .. } => editor.overlay.marquee = Some((view.axis, start, world)),
                VertexPointer::Move3D { .. } => {}
            }
        }
        if response.drag_stopped() {
            self.vertex_drag = None;
            match drag {
                VertexPointer::Marquee { .. } => {
                    if let Some((axis, a, b)) = editor.overlay.marquee.take() {
                        let (low, high) = (a.min(b), a.max(b));
                        editor.vertex_select_where(
                            |point| {
                                let at = axis.project(point);
                                at.cmpge(low).all() && at.cmple(high).all()
                            },
                            pointer.modifiers.ctrl,
                        );
                    }
                }
                _ => editor.end_vertex_drag(),
            }
        }
    }

    /// A click in a 2D view: selects the handles under it, or else the brush under it.
    /// Clicking empty space deselects the vertices but keeps the brushes.
    fn vertex_click_2d(
        &mut self,
        view: &View2D,
        rect: Rect,
        position: Pos2,
        editor: &mut Editor,
        pointer: &PointerState,
    ) {
        let project = |point: DVec3| Some(view.to_screen(rect, view.axis.project(point)));
        let hits = editor.vertex.hits(project, position, REACH_PIXELS, true);
        if !hits.is_empty() {
            editor.vertex_pick(hits, pointer.modifiers.ctrl);
            return;
        }
        let tolerance = pick_tolerance(editor) / view.scale;
        match editor
            .scene
            .pick_2d(view.axis, view.to_world(rect, position), tolerance, editor.solid_picks())
        {
            Some(owner) if !editor.selection.contains(&owner) || pointer.modifiers.ctrl => {
                pick(editor, Some(owner), pointer.modifiers)
            }
            Some(_) => {}
            None => editor.vertex_pick(Vec::new(), pointer.modifiers.ctrl),
        }
    }

    /// Runs the vertex tool in a 3D view. Returns whether the mouse is on a handle or
    /// dragging one, so the click does not also select a brush.
    pub(super) fn vertex_tool_3d(
        &mut self,
        slot: usize,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) -> bool {
        if editor.tool != Tool::Vertex || editor.camera.mouse_look() {
            return false;
        }
        let hits_at = |editor: &Editor, at: Pos2| {
            editor
                .vertex
                .hits(|point| editor.camera.project(rect, point), at, REACH_PIXELS, false)
        };
        if response.drag_started_by(PointerButton::Primary) {
            if let Some(origin) = pointer.press_origin {
                let hits = hits_at(editor, origin);
                if let Some(anchor) = hits.first().map(|hit| hit.position()) {
                    editor.vertex_pick(hits, pointer.modifiers.ctrl);
                    editor.begin_vertex_drag();
                    self.vertex_drag = Some(VertexPointer::Move3D {
                        slot,
                        anchor,
                        vertical: pointer.modifiers.shift,
                    });
                }
            }
        }
        if let Some(VertexPointer::Move3D {
            slot: dragged,
            anchor,
            vertical,
        }) = self.vertex_drag
        {
            if dragged == slot {
                response.ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                if let Some(position) = pointer.position {
                    let (origin, direction) = editor.camera.ray(rect, position);
                    let target = drag_target(&editor.camera, origin, direction, anchor, vertical, editor.snap_step());
                    if let Some(target) = target {
                        editor.drag_vertices(target - anchor);
                    }
                }
                if response.drag_stopped() {
                    self.vertex_drag = None;
                    editor.end_vertex_drag();
                }
                return true;
            }
        }
        let hovered = response
            .hover_pos()
            .map(|position| hits_at(editor, position))
            .unwrap_or_default();
        if hovered.is_empty() {
            return false;
        }
        response.ctx.set_cursor_icon(egui::CursorIcon::Grab);
        if response.clicked_by(PointerButton::Primary) {
            editor.vertex_pick(hovered, pointer.modifiers.ctrl);
        }
        true
    }

    /// The question asked when a drag left corners on top of each other.
    pub(super) fn vertex_merge_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !editor.vertex.merge_prompt {
            return;
        }
        let mut answer = None;
        egui::Window::new("Merge vertices")
            .id(egui::Id::new("vertex_merge"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Some vertices were dragged onto other vertices.");
                ui.label("Merge them into one?");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Merge").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Move back").clicked() {
                        answer = Some(false);
                    }
                });
            });
        let keys = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::Enter),
                input.key_pressed(egui::Key::Escape),
            )
        });
        match (answer, keys) {
            (Some(merge), _) => editor.answer_vertex_merge(merge),
            (None, (true, _)) => editor.answer_vertex_merge(true),
            (None, (_, true)) => editor.answer_vertex_merge(false),
            _ => {}
        }
    }
}

/// Where a corner grabbed at `anchor` goes when the mouse ray is at `origin` and
/// `direction`: on the floor through it, or straight up and down with `vertical`, snapped
/// to the grid.
fn drag_target(
    camera: &Camera3D,
    origin: DVec3,
    direction: DVec3,
    anchor: DVec3,
    vertical: bool,
    grid: f64,
) -> Option<DVec3> {
    let normal = if vertical {
        let forward = camera.forward();
        DVec3::new(forward.x, forward.y, 0.0).try_normalize()?
    } else {
        DVec3::Z
    };
    let facing = direction.dot(normal);
    if facing.abs() < 1e-6 {
        return None;
    }
    let distance = (anchor - origin).dot(normal) / facing;
    if distance <= 0.0 {
        return None;
    }
    let hit = origin + direction * distance;
    let snapped = (hit / grid).round() * grid;
    Some(if vertical {
        DVec3::new(anchor.x, anchor.y, snapped.z)
    } else {
        DVec3::new(snapped.x, snapped.y, anchor.z)
    })
}

/// Draws the vertex tool's handles and any refused shape in a 2D view.
pub(super) fn draw_vertex_2d(canvas: &mut dyn Canvas, view: &View2D, rect: Rect, editor: &Editor) {
    if editor.tool != Tool::Vertex {
        return;
    }
    let to_screen = |point: DVec3| view.to_screen(rect, view.axis.project(point));
    let outline: Vec<[Pos2; 2]> = editor
        .vertex
        .invalid
        .iter()
        .flat_map(|face| loop_segments(face).map(|[a, b]| [to_screen(a), to_screen(b)]))
        .collect();
    canvas.lines(&outline, INVALID_COLOR);
    draw_handles(canvas, editor, |point| Some(to_screen(point)));
}

/// Draws the vertex tool's handles and any refused shape, filled red, in a 3D view.
pub(super) fn draw_vertex_3d(painter: &egui::Painter, camera: &Camera3D, rect: Rect, editor: &Editor) {
    if editor.tool != Tool::Vertex {
        return;
    }
    for face in &editor.vertex.invalid {
        let points: Option<Vec<Pos2>> = face.iter().map(|point| camera.project(rect, *point)).collect();
        if let Some(points) = points {
            painter.add(egui::Shape::convex_polygon(
                points,
                INVALID_FILL,
                Stroke::new(1.0_f32, INVALID_COLOR),
            ));
        }
    }
    let canvas = &mut crate::canvas::PainterCanvas(painter);
    draw_handles(canvas, editor, |point| camera.project(rect, point));
}

fn draw_handles(canvas: &mut dyn Canvas, editor: &Editor, project: impl Fn(DVec3) -> Option<Pos2>) {
    let mut handles = editor.vertex.handles();
    handles.sort_by_key(|handle| (!handle.picked.is_edge(), handle.selected));
    for handle in handles {
        let Some(at) = project(handle.picked.position()) else {
            continue;
        };
        let (half, color) = match (handle.picked.is_edge(), handle.selected) {
            (_, true) => (
                if handle.picked.is_edge() {
                    EDGE_HALF_SIZE
                } else {
                    VERTEX_HALF_SIZE
                },
                SELECTED_COLOR,
            ),
            (true, false) => (EDGE_HALF_SIZE, EDGE_COLOR),
            (false, false) => (VERTEX_HALF_SIZE, VERTEX_COLOR),
        };
        canvas.fill(
            Rect::from_center_size(at, egui::Vec2::splat(half * 2.0 + 2.0)),
            Color32::BLACK,
        );
        canvas.fill(Rect::from_center_size(at, egui::Vec2::splat(half * 2.0)), color);
    }
}

/// The edges of a face loop.
fn loop_segments(face: &[DVec3]) -> impl Iterator<Item = [DVec3; 2]> + '_ {
    (0..face.len()).map(move |index| [face[index], face[(index + 1) % face.len()]])
}
