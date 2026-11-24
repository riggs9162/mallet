//! The Selection tool's paint select: while Ctrl and the left mouse button are held, every
//! object the cursor passes over joins the selection, in the 2D views and the 3D views. A
//! stroke whose first object is already selected takes objects off the selection instead,
//! so a Ctrl+click toggles one object as in Hammer. In mouselook the crosshair does the
//! picking. In a 2D view a stroke starts only on an object outside the selection box, so a
//! Ctrl drag from empty space still draws a marquee that adds to the selection, and in a 3D
//! view a drag that starts on the gizmo still drags the gizmo.

use std::collections::BTreeSet;

use eframe::egui::{Pos2, Rect, Response};

use super::{pick_tolerance, Drag, MalletApp, PointerState};
use crate::editor::{Editor, SelectMode, Tool};
use crate::scene::Owner;
use crate::view2d::View2D;

/// How a stroke picks along the path the cursor took since the last frame: at points at
/// most `pixels` apart, so a quick swipe does not jump over thin brushes, and at no more
/// than `most` points a frame.
#[derive(Clone, Copy)]
struct Sampling {
    pixels: f32,
    most: usize,
}

/// 2D picks are cheap, so the 2D views pick densely.
const SAMPLING_2D: Sampling = Sampling { pixels: 4.0, most: 32 };
/// A 3D pick casts a ray at every brush, prop and displacement, so the 3D views pick less
/// often.
const SAMPLING_3D: Sampling = Sampling { pixels: 8.0, most: 8 };

/// A paint select stroke in progress.
pub(super) struct PaintSelect {
    /// The viewport slot the stroke started in.
    slot: usize,
    /// Where the cursor was on the previous frame.
    last: Pos2,
    /// What the stroke has picked so far, so each object is only added once.
    added: BTreeSet<Owner>,
    /// Whether the stroke adds to or takes from the selection, decided by the first object
    /// it picks; `None` until it picks one.
    mode: Option<SelectMode>,
}

/// What one frame of a viewport tells paint select.
struct StrokeFrame {
    /// Whether the left button is held down on the viewport.
    held: bool,
    /// Whether Ctrl is held.
    ctrl: bool,
    /// Where a new stroke would start, when one may start in the viewport now.
    start: Option<Pos2>,
    /// Where the cursor picks.
    cursor: Option<Pos2>,
    /// How densely the viewport picks along the cursor's path.
    sampling: Sampling,
}

impl MalletApp {
    /// Whether a paint select stroke is going on in `slot`.
    pub(super) fn paint_selecting(&self, slot: usize) -> bool {
        self.paint_select.as_ref().is_some_and(|stroke| stroke.slot == slot)
    }

    /// Ends a stroke once the Selection tool is no longer the active tool.
    pub(super) fn end_paint_select_with_tool(&mut self, editor: &Editor) {
        if editor.tool != Tool::Select {
            self.paint_select = None;
        }
    }

    /// Runs paint select in a 2D view for one frame. A stroke starts when the left button
    /// goes down with Ctrl held on an object anywhere but on the selection box, its handles
    /// and the pivot, so a Ctrl drag from empty space still draws an adding marquee. Returns
    /// whether a stroke is going on in this view, so the view's own clicks and drags leave
    /// it alone until the button comes up.
    pub(super) fn paint_select_2d(
        &mut self,
        slot: usize,
        view: &View2D,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) -> bool {
        let held = response.is_pointer_button_down_on() && response.ctx.input(|input| input.pointer.primary_down());
        let tolerance = pick_tolerance(editor) / view.scale;
        let pick = |editor: &Editor, point: Pos2| {
            editor
                .scene
                .pick_2d(view.axis, view.to_world(rect, point), tolerance, editor.solid_picks())
        };
        let start = pointer.press_origin.filter(|origin| {
            self.drag.is_none()
                && pointer.modifiers.ctrl
                && matches!(Self::begin_drag(slot, view, rect, *origin, editor, false), Drag::Marquee { .. })
                && pick(editor, *origin).is_some()
        });
        let frame = StrokeFrame {
            held,
            ctrl: pointer.modifiers.ctrl,
            start,
            cursor: pointer.position,
            sampling: SAMPLING_2D,
        };
        self.paint_select(slot, editor, frame, pick)
    }

    /// Runs paint select in a 3D view for one frame. A stroke starts when the left button
    /// goes down with Ctrl held anywhere but on the gizmo, which `on_gizmo` tells. Returns
    /// whether a stroke is going on in this view, so the view's own clicks leave it alone
    /// until the button comes up.
    pub(super) fn paint_select_3d(
        &mut self,
        slot: usize,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
        on_gizmo: bool,
    ) -> bool {
        let looking = editor.camera.mouse_look();
        let held = if looking {
            self.look_buttons.down()[0]
        } else {
            response.is_pointer_button_down_on() && response.ctx.input(|input| input.pointer.primary_down())
        };
        let cursor = if looking { Some(rect.center()) } else { pointer.position };
        let free = !on_gizmo && self.drag.is_none() && self.gizmo_drag.is_none();
        let frame = StrokeFrame {
            held,
            ctrl: pointer.modifiers.ctrl,
            start: cursor.filter(|_| free && editor.tool == Tool::Select),
            cursor,
            sampling: SAMPLING_3D,
        };
        self.paint_select(slot, editor, frame, |editor, point| {
            let (origin, direction) = editor.camera.ray(rect, point);
            editor
                .scene
                .pick_ray(origin, direction, &editor.models, editor.solid_picks())
        })
    }

    /// Starts, follows or ends the stroke in `slot` for one frame, picking with `pick` at
    /// points along the cursor's path and adding what it finds to the selection, or taking
    /// it off when the stroke's first object was already selected.
    fn paint_select(
        &mut self,
        slot: usize,
        editor: &mut Editor,
        frame: StrokeFrame,
        pick: impl Fn(&Editor, Pos2) -> Option<Owner>,
    ) -> bool {
        let points: Vec<Pos2> = match self.paint_select.as_mut() {
            Some(stroke) if stroke.slot != slot => return false,
            Some(_) if !frame.held => {
                self.paint_select = None;
                return true;
            }
            Some(stroke) => match frame.cursor {
                Some(cursor) => {
                    let from = std::mem::replace(&mut stroke.last, cursor);
                    if frame.ctrl {
                        path(from, cursor, frame.sampling).collect()
                    } else {
                        Vec::new()
                    }
                }
                None => Vec::new(),
            },
            None => {
                let Some(start) = frame.start.filter(|_| frame.held && frame.ctrl) else {
                    return false;
                };
                let cursor = frame.cursor.unwrap_or(start);
                self.paint_select = Some(PaintSelect {
                    slot,
                    last: cursor,
                    added: BTreeSet::new(),
                    mode: None,
                });
                std::iter::once(start)
                    .chain(path(start, cursor, frame.sampling))
                    .collect()
            }
        };
        let Some(stroke) = self.paint_select.as_mut() else {
            return true;
        };
        let picked: Vec<Owner> = points
            .into_iter()
            .filter_map(|point| pick(editor, point))
            .filter(|owner| stroke.added.insert(*owner))
            .collect();
        let Some(first) = picked.first() else {
            return true;
        };
        let mode = *stroke.mode.get_or_insert(if editor.selection.contains(first) {
            SelectMode::Remove
        } else {
            SelectMode::Add
        });
        editor.select_picked(picked, mode);
        true
    }
}

/// Points along the cursor's path from `from` to `to`, spaced as `sampling` says and ending
/// at `to`.
fn path(from: Pos2, to: Pos2, sampling: Sampling) -> impl Iterator<Item = Pos2> {
    let steps = ((from.distance(to) / sampling.pixels).ceil() as usize).clamp(1, sampling.most);
    (1..=steps).map(move |step| from.lerp(to, step as f32 / steps as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_ends_at_the_cursor_and_keeps_its_points_close() {
        let points: Vec<Pos2> = path(Pos2::new(0.0, 0.0), Pos2::new(20.0, 0.0), SAMPLING_2D).collect();
        assert_eq!(points.len(), 5);
        assert_eq!(points.last(), Some(&Pos2::new(20.0, 0.0)));
        assert!(points
            .windows(2)
            .all(|pair| pair[0].distance(pair[1]) <= SAMPLING_2D.pixels + 1e-4));
    }

    #[test]
    fn a_still_cursor_picks_once_and_a_long_swipe_is_capped() {
        assert_eq!(path(Pos2::new(5.0, 5.0), Pos2::new(5.0, 5.0), SAMPLING_3D).count(), 1);
        assert_eq!(path(Pos2::ZERO, Pos2::new(10_000.0, 0.0), SAMPLING_2D).count(), SAMPLING_2D.most);
        assert_eq!(path(Pos2::ZERO, Pos2::new(10_000.0, 0.0), SAMPLING_3D).count(), SAMPLING_3D.most);
    }
}
