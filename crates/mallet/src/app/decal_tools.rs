//! The Overlay tool in the 3D view: the handles on the corners of the selected overlay
//! and dragging them across the overlay's plane.

use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Response};
use glam::DVec3;
use mallet_vmf::overlay::Overlay;

use super::{MalletApp, PointerState};
use crate::canvas::{Canvas, PainterCanvas};
use crate::decals::CornerDrag;
use crate::editor::{Editor, Tool};
use crate::scene::next_generation;
use crate::view3d::Camera3D;

/// How close, in pixels, a press has to be to a corner handle to grab it.
const REACH_PIXELS: f32 = 7.0;
/// Half the side length, in pixels, of a corner handle.
const HANDLE_HALF_SIZE: f32 = 3.5;
const HANDLE_COLOR: Color32 = Color32::from_rgb(240, 240, 240);
const HOT_COLOR: Color32 = Color32::from_rgb(255, 200, 60);

/// A corner of an overlay being dragged in a 3D view.
#[derive(Clone, Copy, Debug)]
pub(super) struct CornerPointer {
    slot: usize,
    entity: u32,
    corner: usize,
    /// The undo group of the drag's steps.
    group: u64,
    /// Whether only this corner moves, because Shift was held when the drag began.
    free: bool,
}

/// The corner handle under a screen position, if any: the nearest within reach.
fn corner_at(camera: &Camera3D, rect: Rect, placed: &Overlay, at: Pos2) -> Option<usize> {
    placed
        .world_corners()
        .iter()
        .enumerate()
        .filter_map(|(index, corner)| Some((index, camera.project(rect, *corner)?.distance(at))))
        .filter(|(_, distance)| *distance <= REACH_PIXELS)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// Where a mouse ray meets an overlay's plane.
fn on_plane(placed: &Overlay, origin: DVec3, direction: DVec3) -> Option<DVec3> {
    let facing = direction.dot(placed.normal);
    if facing.abs() < 1e-6 {
        return None;
    }
    let distance = (placed.origin - origin).dot(placed.normal) / facing;
    (distance > 0.0).then(|| origin + direction * distance)
}

impl MalletApp {
    /// Runs the Overlay tool's corner handles in a 3D view. Returns whether the mouse is
    /// on a handle or dragging one, so the click does not also place an overlay.
    pub(super) fn overlay_tool_3d(
        &mut self,
        slot: usize,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) -> bool {
        if editor.tool != Tool::Overlay || editor.camera.mouse_look() {
            self.overlay_drag = None;
            return false;
        }
        let Some((entity, placed)) = editor.handled_overlay() else {
            self.overlay_drag = None;
            return false;
        };
        if response.drag_started_by(PointerButton::Primary) {
            let grabbed = pointer
                .press_origin
                .and_then(|origin| corner_at(&editor.camera, rect, &placed, origin));
            if let Some(corner) = grabbed {
                self.overlay_drag = Some(CornerPointer {
                    slot,
                    entity,
                    corner,
                    group: next_generation(),
                    free: pointer.modifiers.shift,
                });
            }
        }
        if let Some(drag) = self
            .overlay_drag
            .filter(|drag| drag.slot == slot && drag.entity == entity)
        {
            response.ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            if let Some(position) = pointer.position {
                let (origin, direction) = editor.camera.build_ray(rect, position);
                if let Some(point) = on_plane(&placed, origin, direction) {
                    let how = if drag.free {
                        CornerDrag::Free
                    } else {
                        CornerDrag::Rectangle
                    };
                    if let Err(error) = editor.move_overlay_corner(entity, drag.corner, point, how, drag.group) {
                        editor.message = error;
                    }
                }
            }
            if response.drag_stopped() {
                self.overlay_drag = None;
            }
            return true;
        }
        let hovered = response
            .hover_pos()
            .and_then(|position| corner_at(&editor.camera, rect, &placed, position));
        if hovered.is_some() {
            response.ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
        hovered.is_some()
    }
}

/// Draws a handle on each corner of the overlay the Overlay tool has selected.
pub(super) fn draw_overlay_handles(painter: &egui::Painter, camera: &Camera3D, rect: Rect, editor: &Editor) {
    if editor.tool != Tool::Overlay {
        return;
    }
    let Some((_, placed)) = editor.handled_overlay() else {
        return;
    };
    let hot = painter
        .ctx()
        .pointer_hover_pos()
        .filter(|position| rect.contains(*position))
        .and_then(|position| corner_at(camera, rect, &placed, position));
    let canvas = &mut PainterCanvas(painter);
    for (index, corner) in placed.world_corners().iter().enumerate() {
        let Some(at) = camera.project(rect, *corner) else {
            continue;
        };
        let color = if hot == Some(index) { HOT_COLOR } else { HANDLE_COLOR };
        canvas.fill(
            Rect::from_center_size(at, egui::Vec2::splat(HANDLE_HALF_SIZE * 2.0 + 2.0)),
            Color32::BLACK,
        );
        canvas.fill(
            Rect::from_center_size(at, egui::Vec2::splat(HANDLE_HALF_SIZE * 2.0)),
            color,
        );
    }
}
