//! Hammer's View menu and viewport keys: centring the views on the selection, going to
//! a brush or a point, switching and maximizing viewports and preset 2D zooms.

use std::collections::BTreeSet;

use glam::DVec3;
use mallet_vmf::geometry::Bounds;

use super::{Editor, SelectMode, ViewMode};
use crate::scene::Owner;

/// How far around a point Go to Coordinates frames the 3D view.
const GO_TO_REACH: f64 = 128.0;
/// The factor 1 and 2 change the 3D view's draw distance by, as Hammer's far clipping
/// plane keys do.
const DRAW_DISTANCE_STEP: f64 = 1.25;
/// The range of the 3D view's draw distance.
const DRAW_DISTANCE_RANGE: (f64, f64) = (256.0, 65536.0);

/// Counts for Hammer's Map > Show Information dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapInformation {
    pub solids: usize,
    pub faces: usize,
    pub point_entities: usize,
    pub solid_entities: usize,
    pub unique_textures: usize,
    /// The materials the map's faces use, sorted.
    pub textures: Vec<String>,
}

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

    /// Hammer's Go to Brush Number: selects the brush with this id, or the brush entity
    /// it belongs to, and centres every view on it.
    pub fn go_to_brush(&mut self, id: u32) -> Result<(), String> {
        let document = self.document.as_ref().ok_or("no map is open")?;
        let owner = document
            .vmf
            .all_entities()
            .find_map(|entity| {
                entity.solids().find(|solid| solid.id() == Some(id))?;
                match entity.classname() {
                    "worldspawn" => Some(Owner::Solid(id)),
                    _ => entity.id().map(Owner::Entity),
                }
            })
            .ok_or_else(|| format!("no brush has number {id}"))?;
        self.select([owner], SelectMode::Replace);
        self.center_2d_on_selection();
        self.center_3d_on_selection();
        self.message = format!("Brush {id}");
        Ok(())
    }

    /// Hammer's Go to Coordinates: centres the 2D views on a point and points the camera
    /// at it.
    pub fn go_to_point(&mut self, point: DVec3) {
        self.center_2d_on(point);
        self.camera.look_at(&Bounds {
            min: point - DVec3::splat(GO_TO_REACH),
            max: point + DVec3::splat(GO_TO_REACH),
        });
        self.message = format!("Centred on {} {} {}", point.x, point.y, point.z);
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

    /// The counts Hammer's Map > Show Information lists.
    pub fn map_information(&self) -> MapInformation {
        let Some(document) = &self.document else {
            return MapInformation::default();
        };
        let mut info = MapInformation::default();
        let mut textures = BTreeSet::new();
        for entity in document.vmf.all_entities() {
            let solids = entity.solids().count();
            if entity.classname() != "worldspawn" {
                if solids == 0 {
                    info.point_entities += 1;
                } else {
                    info.solid_entities += 1;
                }
            }
            for solid in entity.solids() {
                info.solids += 1;
                for side in solid.sides() {
                    info.faces += 1;
                    textures.insert(side.material.to_ascii_uppercase());
                }
            }
        }
        info.unique_textures = textures.len();
        info.textures = textures.into_iter().collect();
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_to_brush_selects_it_and_information_counts_it() {
        let mut editor = Editor::default();
        editor.new_map();
        let id = editor.create_block(DVec3::ZERO, DVec3::splat(64.0)).unwrap();
        editor.clear_selection();
        editor.go_to_brush(id).unwrap();
        assert!(editor.selection.contains(&Owner::Solid(id)));
        assert!(editor.go_to_brush(id + 100).is_err());
        let info = editor.map_information();
        assert_eq!((info.solids, info.faces, info.unique_textures), (1, 6, 1));
        editor.preset_zoom(0, 5);
        assert_eq!(editor.views[0].scale, 1.0);
    }
}
