//! The Selection tool's mouse handling that both kinds of view share, and its 3D side.
//!
//! Moves are dragged live: every frame the drag moves, the selection itself is moved by
//! the change since the last frame, each step an undo step of its own, and letting go
//! joins those steps into one. The move comes from a [`TranslationPlane`] fixed when the
//! drag starts, in the 3D views and the 2D views alike, so snapping and axis constraints
//! work the same everywhere.
//!
//! In a 3D view the left button picks through a ray from the camera. A press on an object
//! selects it at once (Ctrl toggles, which paint select handles, and Shift adds); holding
//! the button still steps the selection to the next object behind it along the same ray
//! every half second; moving the mouse past the drag threshold drags the selection along a
//! plane through the point pressed. Letting go of a press on an object that was already
//! selected, without moving, switches the gizmo between moving and scaling. A press on
//! empty space draws a rubber band instead, and letting go selects everything whose
//! bounds reach into the volume the band cuts out of the view.

use std::collections::BTreeSet;

use eframe::egui::{self, Modifiers, Pos2, Rect, Response};
use glam::DVec3;

use super::{MalletApp, PointerState, DRAG_THRESHOLD_PIXELS};
use crate::editor::{Editor, SelectMode};
use crate::gizmo::Part as GizmoPart;
use crate::scene::Owner;
use crate::translation_plane::{Constraint, TranslationPlane};
use crate::view3d::{Camera3D, NEAR};

/// Seconds between steps to the next object behind while the button is held still.
const CYCLE_SECONDS: f64 = 0.5;
/// World units the debug drawing of the drag plane reaches out from where the drag began.
const DEBUG_PLANE_REACH: f64 = 512.0;
/// Grid lines across the debug drawing of the drag plane, each way.
const DEBUG_PLANE_LINES: usize = 8;

/// Where a drag happens: a viewport slot showing the 3D camera, or one of the 2D views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Site {
    Slot(usize),
    View(usize),
}

/// How a click or a rubber band changes the selection: Ctrl toggles, Shift adds, and
/// otherwise it replaces.
pub(super) fn select_mode(modifiers: Modifiers) -> SelectMode {
    if modifiers.ctrl {
        SelectMode::Toggle
    } else if modifiers.shift {
        SelectMode::Add
    } else {
        SelectMode::Replace
    }
}

/// Whether no modifier key is held.
fn plain(modifiers: Modifiers) -> bool {
    !modifiers.ctrl && !modifiers.shift && !modifiers.alt
}

/// A move of the selection being dragged, applied to the map as it goes.
pub(super) struct LiveMove {
    pub site: Site,
    plane: TranslationPlane,
    constraint: Constraint,
    /// The move applied so far, from where the drag started.
    applied: DVec3,
    /// Undo steps the drag has made, which letting go joins into one.
    steps: usize,
    /// The selection before the drag, selected again if it is cancelled.
    originals: BTreeSet<Owner>,
    /// Whether the drag moves a copy of the selection, made when it started.
    cloned: bool,
    /// The gizmo part being dragged, if the drag started on one, so it stays highlighted.
    pub part: Option<GizmoPart>,
}

impl LiveMove {
    /// Starts dragging the selection along `plane`, whose origin is where the drag began.
    /// With `clone` the selection is copied first and the copy moves, leaving the
    /// originals where they are. `None` when nothing is selected or the copy fails.
    pub fn begin(
        editor: &mut Editor,
        site: Site,
        plane: TranslationPlane,
        constraint: Constraint,
        clone: bool,
        part: Option<GizmoPart>,
    ) -> Option<Self> {
        if editor.selection.is_empty() {
            return None;
        }
        let originals = editor.selection.clone();
        let mut steps = 0;
        if clone {
            if let Err(error) = editor.clone_selection(DVec3::ZERO) {
                editor.message = error;
                return None;
            }
            steps = 1;
        }
        Some(Self {
            site,
            plane,
            constraint,
            applied: DVec3::ZERO,
            steps,
            originals,
            cloned: clone,
            part,
        })
    }

    /// The plane the drag slides along.
    pub fn plane(&self) -> &TranslationPlane {
        &self.plane
    }

    /// Moves the selection so it follows a ray: where the ray meets the plane, snapped to
    /// the grid and kept to the constraint, less what was already applied.
    pub fn follow(&mut self, editor: &mut Editor, origin: DVec3, direction: DVec3) {
        let step = editor.snap_step();
        let Some(wanted) = self.plane.translation(origin, direction, step, self.constraint) else {
            return;
        };
        if wanted == self.applied {
            return;
        }
        match editor.move_selection(wanted - self.applied) {
            Ok(()) => {
                self.steps += 1;
                self.applied = wanted;
            }
            Err(error) => editor.message = error,
        }
    }

    /// Ends the drag: its steps become one undo step. A copy that was never moved is
    /// taken back, so a Shift+click does not leave a duplicate behind.
    pub fn commit(self, editor: &mut Editor) {
        if self.applied == DVec3::ZERO {
            if self.cloned {
                editor.cancel_live_steps(self.steps, self.originals);
                editor.message = "Nothing was moved".to_owned();
            }
            return;
        }
        editor.commit_live_steps(self.steps);
        let offset = mallet_vmf::format_vec3(self.applied);
        editor.message = if self.cloned {
            let count = editor.selection.len();
            let noun = if count == 1 { "object" } else { "objects" };
            format!("Cloned {count} {noun} and moved them by {offset}")
        } else {
            format!("Moved selection by {offset}")
        };
    }

    /// Cancels the drag: undoes what it moved or copied and selects the originals again.
    pub fn cancel(self, editor: &mut Editor) {
        editor.cancel_live_steps(self.steps, self.originals);
    }
}

/// A left-button press with the Selection tool in a 3D view that is not yet a drag.
pub(super) enum Press3D {
    /// On an object, which the press selected or which already was.
    Object {
        slot: usize,
        origin: Pos2,
        /// What the ray through the press hits, nearest first, with its distance.
        hits: Vec<(Owner, f64)>,
        /// Which hit is selected now; holding the button steps it further back.
        index: usize,
        /// Where the ray meets the selected hit, which a drag's plane goes through.
        point: DVec3,
        /// Whether the hit was selected before the press.
        was_selected: bool,
        modifiers: Modifiers,
        /// When the selection next steps to the object behind.
        next_cycle: f64,
        /// Whether holding the button has stepped through objects already.
        cycled: bool,
    },
    /// On empty space.
    Empty { slot: usize, origin: Pos2, modifiers: Modifiers },
    /// A rubber band being dragged from empty space.
    Marquee { slot: usize, origin: Pos2, modifiers: Modifiers },
}

impl Press3D {
    fn slot(&self) -> usize {
        match self {
            Self::Object { slot, .. } | Self::Empty { slot, .. } | Self::Marquee { slot, .. } => *slot,
        }
    }

    fn origin(&self) -> Pos2 {
        match self {
            Self::Object { origin, .. } | Self::Empty { origin, .. } | Self::Marquee { origin, .. } => *origin,
        }
    }
}

/// The planes, facing inwards, of the part of the camera's view that a screen rectangle
/// between `a` and `b` covers, from the near plane out to the draw distance.
pub(super) fn marquee_volume(camera: &Camera3D, rect: Rect, a: Pos2, b: Pos2) -> Vec<(DVec3, f64)> {
    let (min, mut max) = (a.min(b), a.max(b));
    max.x = max.x.max(min.x + 1.0);
    max.y = max.y.max(min.y + 1.0);
    let ray = |point: Pos2| camera.build_ray(rect, point).1;
    let corners = [
        ray(min),
        ray(Pos2::new(max.x, min.y)),
        ray(max),
        ray(Pos2::new(min.x, max.y)),
    ];
    let inside = ray(min.lerp(max, 0.5));
    let eye = camera.position;
    let mut planes: Vec<(DVec3, f64)> = (0..4)
        .map(|index| {
            let normal = corners[index].cross(corners[(index + 1) % 4]).normalize_or_zero();
            let normal = if normal.dot(inside) < 0.0 { -normal } else { normal };
            (normal, normal.dot(eye))
        })
        .collect();
    let forward = camera.forward();
    planes.push((forward, forward.dot(eye) + NEAR));
    planes.push((-forward, -(forward.dot(eye) + camera.far)));
    planes
}

impl MalletApp {
    /// Follows a live move being dragged in a 3D view, ending it when the left button
    /// comes up. Returns whether one is going on in this slot.
    pub(super) fn live_move_3d(&mut self, slot: usize, rect: Rect, editor: &mut Editor, pointer: &PointerState) -> bool {
        if self.live_move.as_ref().is_none_or(|live| live.site != Site::Slot(slot)) {
            return false;
        }
        let down = pointer.primary_down;
        if !down {
            if let Some(live) = self.live_move.take() {
                live.commit(editor);
            }
            return true;
        }
        if let (Some(live), Some(position)) = (self.live_move.as_mut(), pointer.position) {
            let (origin, direction) = editor.camera.build_ray(rect, position);
            live.follow(editor, origin, direction);
        }
        true
    }

    /// Runs the Selection tool's left button in a 3D view for one frame, outside
    /// mouselook and off the gizmo: picking, stepping through stacked objects, dragging
    /// the selection and the rubber band.
    pub(super) fn select_3d(
        &mut self,
        slot: usize,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) {
        let (pressed, down, now) = response
            .ctx
            .input(|input| (input.pointer.primary_pressed(), input.pointer.primary_down(), input.time));
        if self.press_3d.is_none() && pressed && response.hovered() {
            self.begin_press_3d(slot, rect, editor, pointer, now);
        }
        let Some(press) = self.press_3d.as_mut().filter(|press| press.slot() == slot) else {
            return;
        };
        let moved = pointer
            .position
            .is_some_and(|position| position.distance(press.origin()) > DRAG_THRESHOLD_PIXELS);
        match press {
            Press3D::Object {
                hits,
                index,
                point,
                was_selected,
                modifiers,
                next_cycle,
                cycled,
                ..
            } => {
                if !down {
                    let double = response.double_clicked_by(egui::PointerButton::Primary);
                    if double {
                        editor.properties_open = true;
                    } else if *was_selected && !*cycled && plain(*modifiers) {
                        editor.toggle_gizmo_scale();
                    }
                    self.press_3d = None;
                } else if moved {
                    let (owner, point, clone) = (hits[*index].0, *point, modifiers.shift);
                    self.press_3d = None;
                    if editor.selection.contains(&owner) {
                        let plane = TranslationPlane::facing(point, editor.camera.forward());
                        self.live_move = LiveMove::begin(editor, Site::Slot(slot), plane, Constraint::Free, clone, None);
                    }
                } else if hits.len() > 1 && now >= *next_cycle {
                    let previous = hits[*index].0;
                    *index = (*index + 1) % hits.len();
                    let (next, distance) = hits[*index];
                    let (origin, direction) = editor.camera.build_ray(rect, press_origin(pointer, rect));
                    *point = origin + direction * distance;
                    *next_cycle = now + CYCLE_SECONDS;
                    *cycled = true;
                    if plain(*modifiers) {
                        editor.select_picked([next], SelectMode::Replace);
                    } else {
                        editor.select_picked([previous], SelectMode::Remove);
                        editor.select_picked([next], SelectMode::Add);
                    }
                    response.ctx.request_repaint_after_secs(CYCLE_SECONDS as f32);
                } else if hits.len() > 1 {
                    response.ctx.request_repaint_after_secs((*next_cycle - now).max(0.0) as f32);
                }
            }
            Press3D::Empty { origin, modifiers, .. } => {
                if !down {
                    if plain(*modifiers) {
                        editor.clear_selection();
                    }
                    self.press_3d = None;
                } else if moved {
                    *press = Press3D::Marquee {
                        slot,
                        origin: *origin,
                        modifiers: *modifiers,
                    };
                }
            }
            Press3D::Marquee { origin, modifiers, .. } => {
                if !down {
                    let (origin, mode) = (*origin, select_mode(*modifiers));
                    self.press_3d = None;
                    if let Some(position) = pointer.position {
                        let planes = marquee_volume(&editor.camera, rect, origin, position);
                        let owners = editor.scene.owners_in_volume(&planes, editor.solid_picks());
                        editor.select_picked(owners, mode);
                    }
                }
            }
        }
    }

    /// Starts a press: picks along the ray through it and selects the nearest hit unless
    /// it is selected already.
    fn begin_press_3d(&mut self, slot: usize, rect: Rect, editor: &mut Editor, pointer: &PointerState, now: f64) {
        let origin = press_origin(pointer, rect);
        let (start, direction) = editor.camera.build_ray(rect, origin);
        let hits = editor
            .scene
            .pick_ray_all(start, direction, &editor.models, editor.solid_picks());
        let modifiers = pointer.modifiers;
        let Some(&(owner, distance)) = hits.first() else {
            self.press_3d = Some(Press3D::Empty { slot, origin, modifiers });
            return;
        };
        let was_selected = editor.selection.contains(&owner);
        if !was_selected || modifiers.ctrl {
            editor.select_picked([owner], select_mode(modifiers));
        }
        self.press_3d = Some(Press3D::Object {
            slot,
            origin,
            hits,
            index: 0,
            point: start + direction * distance,
            was_selected,
            modifiers,
            next_cycle: now + CYCLE_SECONDS,
            cycled: false,
        });
    }

    /// The cursor for the Selection tool in a 3D view: a hand over objects, a move cursor
    /// over selected ones, and the normal arrow over empty space; while dragging, a
    /// grabbing hand or a crosshair for the rubber band.
    pub(super) fn select_cursor_3d(&mut self, slot: usize, rect: Rect, response: &Response, editor: &Editor) {
        if self.live_move.as_ref().is_some_and(|live| live.site == Site::Slot(slot)) {
            response.ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            return;
        }
        if matches!(&self.press_3d, Some(Press3D::Marquee { slot: at, .. }) if *at == slot) {
            response.ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            return;
        }
        let Some(position) = response.hover_pos() else {
            return;
        };
        let key = (slot, position, editor.scene.geometry_generation);
        let hit = match self.hover_pick {
            Some((at, owner)) if at == key => owner,
            _ => {
                let (origin, direction) = editor.camera.build_ray(rect, position);
                let owner = editor
                    .scene
                    .pick_ray(origin, direction, &editor.models, editor.solid_picks());
                self.hover_pick = Some((key, owner));
                owner
            }
        };
        let icon = match hit {
            Some(owner) if editor.selection.contains(&owner) => egui::CursorIcon::Move,
            Some(_) => egui::CursorIcon::PointingHand,
            None => egui::CursorIcon::Default,
        };
        response.ctx.set_cursor_icon(icon);
    }

    /// Draws the rubber band being dragged in a 3D view, and, when Mallet Options asks for
    /// it, the plane a move drag slides along.
    pub(super) fn draw_select_3d(&self, painter: &egui::Painter, slot: usize, rect: Rect, editor: &Editor, pointer: &PointerState) {
        if let (Some(Press3D::Marquee { slot: at, origin, .. }), Some(position)) = (&self.press_3d, pointer.position) {
            if *at == slot {
                let band = Rect::from_two_pos(*origin, position.clamp(rect.min, rect.max));
                let color = crate::style::current().tool;
                painter.rect_filled(band, 0.0, color.gamma_multiply(0.08));
                painter.rect_stroke(band, 0.0, egui::Stroke::new(1.0_f32, color), egui::StrokeKind::Inside);
            }
        }
        let Some(live) = self
            .live_move
            .as_ref()
            .filter(|live| live.site == Site::Slot(slot) && editor.config.view.debug_drag_plane)
        else {
            return;
        };
        draw_plane(painter, &editor.camera, rect, live.plane());
    }
}

/// Where the current press went down in a viewport, or the cursor when egui has no
/// press origin.
fn press_origin(pointer: &PointerState, rect: Rect) -> Pos2 {
    pointer.press_origin.or(pointer.position).unwrap_or(rect.center())
}

/// Draws a drag plane faintly: a grid of lines on it around where the drag began.
fn draw_plane(painter: &egui::Painter, camera: &Camera3D, rect: Rect, plane: &TranslationPlane) {
    let color = egui::Color32::from_rgba_unmultiplied(120, 200, 255, 60);
    let stroke = egui::Stroke::new(1.0_f32, color);
    let step = DEBUG_PLANE_REACH * 2.0 / DEBUG_PLANE_LINES as f64;
    let at = |h: f64, v: f64| plane.origin + plane.horizontal * h + plane.vertical * v;
    for line in 0..=DEBUG_PLANE_LINES {
        let offset = -DEBUG_PLANE_REACH + step * line as f64;
        for [a, b] in [
            [at(offset, -DEBUG_PLANE_REACH), at(offset, DEBUG_PLANE_REACH)],
            [at(-DEBUG_PLANE_REACH, offset), at(DEBUG_PLANE_REACH, offset)],
        ] {
            if let (Some(a), Some(b)) = (camera.project(rect, a), camera.project(rect, b)) {
                painter.line_segment([a, b], stroke);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_choose_how_a_click_selects() {
        assert_eq!(select_mode(Modifiers::NONE), SelectMode::Replace);
        assert_eq!(select_mode(Modifiers::CTRL), SelectMode::Toggle);
        assert_eq!(select_mode(Modifiers::SHIFT), SelectMode::Add);
    }

    #[test]
    fn a_rubber_band_covers_what_is_in_front_of_it() {
        let mut camera = Camera3D::new();
        camera.position = DVec3::ZERO;
        camera.yaw = 0.0;
        camera.pitch = 0.0;
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 400.0));
        let planes = marquee_volume(&camera, rect, Pos2::new(350.0, 150.0), Pos2::new(450.0, 250.0));
        let inside = |point: DVec3| planes.iter().all(|(normal, distance)| normal.dot(point) >= *distance);
        assert!(inside(DVec3::new(500.0, 0.0, 0.0)));
        assert!(!inside(DVec3::new(500.0, 400.0, 0.0)));
        assert!(!inside(DVec3::new(-500.0, 0.0, 0.0)));
        assert!(!inside(DVec3::new(camera.far + 10.0, 0.0, 0.0)));
    }
}
