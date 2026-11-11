//! Hammer's View menu and viewport keys: centring the views on the selection, switching
//! and maximizing viewports and preset 2D zooms.

use super::{Editor, ViewMode};

/// The factor 1 and 2 change the 3D view's draw distance by, as Hammer's far clipping
/// plane keys do.
const DRAW_DISTANCE_STEP: f64 = 1.25;
/// The range of the 3D view's draw distance.
const DRAW_DISTANCE_RANGE: (f64, f64) = (256.0, 65536.0);

impl Editor {
    /// Hammer's Center Views on Selection (Ctrl+E): pans every 2D view to the middle of
    /// the selection, keeping its zoom. With nothing selected it fits the whole map.
    pub fn center_2d_on_selection(&mut self) {
        let bounds = self.scene.selection_bounds();
        if bounds.is_empty() {
            self.fit_2d();
        } else {
            self.center_2d_on(bounds.center());
        }
    }

    /// Hammer's Center 3D Views on Selection: points the camera at the selection from far
    /// enough back to see it all. With nothing selected it frames the whole map.
    pub fn center_3d_on_selection(&mut self) {
        let bounds = self.scene.selection_bounds();
        if bounds.is_empty() {
            self.fit_3d();
        } else {
            self.camera.look_at(&bounds);
        }
    }

    /// Shows a view type in a viewport slot, as F2 to F5 do in Hammer.
    pub fn set_slot_mode(&mut self, slot: usize, mode: ViewMode) {
        if let Some(current) = self.layout.get_mut(slot) {
            *current = mode;
        }
    }

    /// Hammer's Tab in a 2D view: the next 2D view type, top, front then side.
    pub fn cycle_2d_view(&mut self, slot: usize) {
        let next = match self.layout.get(slot) {
            Some(ViewMode::Top) => ViewMode::Front,
            Some(ViewMode::Front) => ViewMode::Side,
            Some(ViewMode::Side) => ViewMode::Top,
            _ => return,
        };
        self.set_slot_mode(slot, next);
    }

    /// Hammer's Maximize/Restore Active View (Shift+Z): a slot fills the viewport area, or
    /// the four views come back when one is maximized already.
    pub fn toggle_maximized(&mut self, slot: usize) {
        self.maximized = match self.maximized {
            Some(_) => None,
            None => Some(slot.min(3)),
        };
    }

    /// Hammer's preset 2D zooms on the number keys: 1 shows a sixteenth of a pixel per
    /// unit, each key after it doubles that, 5 is one pixel per unit and 0 the closest.
    pub fn preset_zoom(&mut self, view: usize, digit: u32) {
        let power = if digit == 0 { 5 } else { digit as i32 - 5 };
        if let Some(view) = self.views.get_mut(view) {
            view.set_scale(2f64.powi(power));
        }
    }

    /// Hammer's 1 and 2 in the 3D view: draw less or more of the map.
    pub fn step_draw_distance(&mut self, further: bool) {
        let distance = &mut self.config.view.draw_distance;
        *distance = if further {
            *distance * DRAW_DISTANCE_STEP
        } else {
            *distance / DRAW_DISTANCE_STEP
        }
        .clamp(DRAW_DISTANCE_RANGE.0, DRAW_DISTANCE_RANGE.1)
        .round();
        self.message = format!("3D draw distance {}", self.config.view.draw_distance);
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_keys_pick_preset_zooms() {
        let mut editor = Editor::default();
        editor.new_map();
        editor.preset_zoom(0, 5);
        assert_eq!(editor.views[0].scale, 1.0);
        editor.preset_zoom(0, 1);
        assert_eq!(editor.views[0].scale, 1.0 / 16.0);
    }
}
