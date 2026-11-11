//! The Camera and Magnify tools.
//!
//! The Camera tool works as in Hammer. In a 3D view a left drag turns the camera and a
//! right drag slides it. In a 2D view each camera shows as a blue circle with a line to
//! the point it looks at: drag from empty space to put the camera there, aimed where the
//! drag ends, or hold Shift to add another camera instead; drag a circle or a line's end to
//! move the camera or re-aim it, with Ctrl to move both. Page Up and Page Down switch
//! cameras and Delete removes the one shown.
//!
//! The Magnify tool zooms a 2D view in on a left click and out on a right click.

use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Response};
use glam::{DVec2, DVec3};

use super::{MalletApp, PointerState};
use crate::editor::{Editor, Tool};
use crate::view2d::View2D;

/// How close, in pixels, a press must be to a camera's circle or aim to drag it.
const REACH_PIXELS: f32 = 7.0;
/// Radius, in pixels, of a camera's circle in the 2D views.
const CIRCLE_PIXELS: f32 = 5.0;
const ACTIVE_COLOR: Color32 = Color32::from_rgb(80, 160, 255);
const OTHER_COLOR: Color32 = Color32::from_rgb(40, 80, 170);

/// A Camera tool drag in a 2D view.
#[derive(Clone, Copy, Debug)]
pub(super) enum CameraDrag {
    /// Moving the shown camera, which keeps aiming at the same point.
    Eye { view: usize },
    /// Re-aiming the shown camera, or aiming one just placed.
    Aim { view: usize },
    /// Moving the shown camera and its aim together (Ctrl).
    Both {
        view: usize,
        start: DVec2,
        eye: DVec3,
        look: DVec3,
    },
}

impl CameraDrag {
    fn view(self) -> usize {
        match self {
            Self::Eye { view } | Self::Aim { view } | Self::Both { view, .. } => view,
        }
    }
}

impl MalletApp {
    /// Runs the Camera tool in a 3D view: a left drag turns the camera, and a right drag
    /// slides it. Only the view being dragged gets the drag, so the shared camera moves once.
    pub(super) fn camera_tool_3d(&mut self, response: &Response, editor: &mut Editor) {
        if editor.tool != Tool::Camera {
            return;
        }
        if response.dragged_by(PointerButton::Primary) {
            editor.camera.look_by(response.drag_delta());
        } else if response.dragged_by(PointerButton::Secondary) {
            editor.camera.pan_by(response.drag_delta());
        }
    }

    /// Runs the Camera tool in a 2D view: placing, adding, moving and aiming cameras.
    pub(super) fn camera_tool_2d(
        &mut self,
        index: usize,
        view: &View2D,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) {
        if response.drag_started_by(PointerButton::Primary) {
            if let Some(origin) = pointer.press_origin {
                self.camera_drag = Some(start_camera_drag(index, view, rect, origin, editor, pointer));
            }
        }
        let Some(drag) = self.camera_drag.filter(|drag| drag.view() == index) else {
            return;
        };
        if let Some(position) = pointer.position {
            let world = view.to_world(rect, position);
            let (list, active) = editor.camera_list();
            let shown = list[active];
            let step = editor.snap_step();
            let snapped = (world / step).round() * step;
            match drag {
                CameraDrag::Eye { .. } => {
                    editor.place_active_camera(on_view(view, snapped, shown.position), shown.look);
                }
                CameraDrag::Aim { .. } => {
                    editor.place_active_camera(shown.position, on_view(view, snapped, shown.position));
                }
                CameraDrag::Both { start, eye, look, .. } => {
                    let offset = view.axis.lift(((world - start) / step).round() * step);
                    editor.place_active_camera(eye + offset, look + offset);
                }
            }
        }
        if response.drag_stopped() {
            self.camera_drag = None;
        }
    }
}

/// What a press at `origin` starts: dragging a camera's circle or aim (switching to that
/// camera first), or placing the shown camera, or with Shift a new one, there.
fn start_camera_drag(
    index: usize,
    view: &View2D,
    rect: Rect,
    origin: Pos2,
    editor: &mut Editor,
    pointer: &PointerState,
) -> CameraDrag {
    let (list, active) = editor.camera_list();
    let near = |point: DVec3| view.to_screen(rect, view.axis.project(point)).distance(origin) <= REACH_PIXELS;
    let order = std::iter::once(active).chain((0..list.len()).filter(|other| *other != active));
    let hit = order
        .filter_map(|camera| {
            let saved = list[camera];
            if near(saved.position) {
                Some((camera, true))
            } else if near(saved.look) {
                Some((camera, false))
            } else {
                None
            }
        })
        .next();
    if let Some((camera, eye)) = hit {
        if camera != active {
            editor.activate_camera(camera);
        }
        let saved = list[camera];
        return match (pointer.modifiers.ctrl, eye) {
            (true, _) => CameraDrag::Both {
                view: index,
                start: view.to_world(rect, origin),
                eye: saved.position,
                look: saved.look,
            },
            (false, true) => CameraDrag::Eye { view: index },
            (false, false) => CameraDrag::Aim { view: index },
        };
    }
    let step = editor.snap_step();
    let start = (view.to_world(rect, origin) / step).round() * step;
    let eye = on_view(view, start, list[active].position);
    if pointer.modifiers.shift {
        editor.add_camera(eye, eye);
    } else {
        editor.place_active_camera(eye, eye);
    }
    CameraDrag::Aim { view: index }
}

/// A point of a 2D view lifted into the world, at the depth of `depth_from`.
fn on_view(view: &View2D, point: DVec2, depth_from: DVec3) -> DVec3 {
    let [horizontal, vertical, _] = view.axis.axes();
    let mut lifted = depth_from;
    lifted[horizontal] = point.x;
    lifted[vertical] = point.y;
    lifted
}

/// Draws every camera in a 2D view while the Camera tool is in use: a circle where it is
/// and a line to the point it looks at, the shown camera brighter than the rest.
pub(super) fn draw_cameras(painter: &egui::Painter, view: &View2D, rect: Rect, editor: &Editor) {
    if editor.tool != Tool::Camera {
        return;
    }
    let (list, active) = editor.camera_list();
    for (index, camera) in list.iter().enumerate() {
        let color = if index == active { ACTIVE_COLOR } else { OTHER_COLOR };
        let eye = view.to_screen(rect, view.axis.project(camera.position));
        let look = view.to_screen(rect, view.axis.project(camera.look));
        let stroke = egui::Stroke::new(1.5_f32, color);
        painter.line_segment([eye, look], stroke);
        painter.circle_stroke(eye, CIRCLE_PIXELS, stroke);
        painter.rect_filled(Rect::from_center_size(look, egui::Vec2::splat(4.0)), 0.0, color);
    }
}
