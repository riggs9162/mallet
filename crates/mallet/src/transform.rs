//! Rotating, scaling, skewing, mirroring, snapping and aligning the selection: what
//! Hammer's selection handles, Transform dialog and Tools menu do, and what the 3D gizmo
//! does in Hammer++.
//!
//! Every command builds one affine matrix and applies it as a single undoable edit:
//! brushes go through [`Edit::Transform`] with the texture lock chosen in the toolbar, and
//! entity origins (and, for rotations and mirrors, angles) through [`Edit::SetKey`].

use glam::{DMat3, DMat4, DVec3};
use mallet_vmf::edit::Edit;
use mallet_vmf::geometry::Bounds;
use mallet_vmf::transform::{self, around, clean, clean_vec3, TextureLock};
use mallet_vmf::{format_vec3, parse_vec3, Solid};
use serde::{Deserialize, Serialize};

use crate::editor::{find_entity, overlay_edits, Editor};
use crate::scene::Owner;

/// What the handles on a selection do in the 2D views. Clicking a selected object again
/// cycles through them, as in Hammer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HandleMode {
    /// Resize from the eight edges and corners.
    #[default]
    Scale,
    /// Rotate from the four corners around the pivot.
    Rotate,
    /// Skew (shear) from the four edges.
    Skew,
}

impl HandleMode {
    /// The mode a click on the selection switches to.
    pub fn next(self) -> Self {
        match self {
            Self::Scale => Self::Rotate,
            Self::Rotate => Self::Skew,
            Self::Skew => Self::Scale,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Scale => "Scale",
            Self::Rotate => "Rotate",
            Self::Skew => "Skew",
        }
    }

    /// The 2D handles that match a 3D gizmo mode, so both kinds of view offer the same
    /// transform. The 2D views move a selection by dragging its body, so a move gizmo
    /// pairs with the scale handles.
    pub fn for_gizmo(mode: GizmoMode) -> Self {
        match mode {
            GizmoMode::Rotate => Self::Rotate,
            GizmoMode::Move | GizmoMode::Scale => Self::Scale,
        }
    }
}

/// What the 3D gizmo does when one of its parts is dragged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GizmoMode {
    /// Arrows move along an axis, squares move in a plane.
    #[default]
    Move,
    /// Rings rotate around an axis.
    Rotate,
    /// Cubes stretch along an axis; the centre cube scales evenly.
    Scale,
}

impl GizmoMode {
    pub const ALL: [GizmoMode; 3] = [Self::Move, Self::Rotate, Self::Scale];

    /// The mode clicking the gizmo's centre switches to.
    pub fn next(self) -> Self {
        match self {
            Self::Move => Self::Rotate,
            Self::Rotate => Self::Scale,
            Self::Scale => Self::Move,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Move => "Move",
            Self::Rotate => "Rotate",
            Self::Scale => "Scale",
        }
    }
}

/// Where a numeric transform turns or scales around.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Pivot {
    /// The centre of the selection's bounds.
    #[default]
    Center,
    /// The pivot placed in the 2D views, or the centre when none is placed.
    Placed,
    /// The world origin.
    Origin,
    /// Any point.
    Point(DVec3),
}

/// A selected entity as far as transforms care.
struct EntityTarget {
    id: u32,
    origin: Option<DVec3>,
    angles: Option<DVec3>,
    /// Point entities have no brushes; they get `angles` when rotated even without the key.
    point: bool,
}

/// Rotation by angles about the X, Y and Z axes, in degrees, applied in that order.
pub fn rotation_xyz(degrees: DVec3) -> DMat3 {
    let [x, y, z] = degrees.to_array().map(f64::to_radians);
    DMat3::from_rotation_z(z) * DMat3::from_rotation_y(y) * DMat3::from_rotation_x(x)
}

/// Rotation in the plane of two world axes, turning `horizontal` towards `vertical` by
/// `radians`, as seen in a 2D view that shows those axes.
pub fn plane_rotation(horizontal: usize, vertical: usize, radians: f64) -> DMat3 {
    let (sin, cos) = radians.sin_cos();
    let mut columns = [DVec3::X, DVec3::Y, DVec3::Z];
    let (h, v) = (columns[horizontal], columns[vertical]);
    columns[horizontal] = h * cos + v * sin;
    columns[vertical] = v * cos - h * sin;
    DMat3::from_cols(columns[0], columns[1], columns[2])
}

/// A shear that moves points along `along` by `amount` for every unit they are from
/// `fixed` along `by`, so the edge at `fixed` stays put.
pub fn shear(along: usize, by: usize, amount: f64, fixed: f64) -> DMat4 {
    let mut linear = DMat3::IDENTITY;
    let mut column = linear.col(by);
    column[along] = amount;
    *linear.col_mut(by) = column;
    let mut offset = DVec3::ZERO;
    offset[along] = -amount * fixed;
    DMat4::from_translation(offset) * DMat4::from_mat3(linear)
}

fn snap(value: f64, grid: f64) -> f64 {
    (value / grid).round() * grid
}

impl Editor {
    /// The point rotations and gizmo drags turn around: the placed pivot, or the centre
    /// of the selection.
    pub fn pivot(&self) -> DVec3 {
        self.overlay
            .pivot
            .unwrap_or_else(|| self.scene.selection_bounds().center())
    }

    /// Resolves a [`Pivot`] choice to a point.
    pub fn pivot_point(&self, pivot: Pivot) -> DVec3 {
        match pivot {
            Pivot::Center => self.scene.selection_bounds().center(),
            Pivot::Placed => self.pivot(),
            Pivot::Origin => DVec3::ZERO,
            Pivot::Point(point) => point,
        }
    }

    /// How brush textures follow a transform under the toolbar's texture lock settings.
    /// Rotations and mirrors carry textures along whole; scaling keeps their scale and
    /// pins them at `anchor` unless texture scale lock is on.
    fn texture_lock(&self, rigid: bool, anchor: DVec3) -> TextureLock {
        let options = &self.config.transform;
        match (options.texture_lock, rigid || options.texture_scale_lock) {
            (false, _) => TextureLock::Off,
            (true, true) => TextureLock::Full,
            (true, false) => TextureLock::Align { anchor },
        }
    }

    /// The brushes and entities a transform of the selection changes.
    fn transform_targets(&self) -> Result<(Vec<u32>, Vec<EntityTarget>), String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        if self.selection.is_empty() {
            return Err("nothing is selected".to_owned());
        }
        let mut solids = Vec::new();
        let mut entities = Vec::new();
        for owner in &self.selection {
            match *owner {
                Owner::Solid(id) => solids.push(id),
                Owner::Entity(id) => {
                    let entity = find_entity(vmf, id)?;
                    let before = solids.len();
                    solids.extend(entity.solids().filter_map(Solid::id));
                    entities.push(EntityTarget {
                        id,
                        origin: entity.origin(),
                        angles: entity.get("angles").and_then(parse_vec3),
                        point: solids.len() == before,
                    });
                }
            }
        }
        Ok((solids, entities))
    }

    /// Applies an affine transform to the selection as one undo step. With `turn` set,
    /// entity angles turn with it, as for rotations and mirrors; scaling and skewing leave
    /// them alone, as Hammer does. `anchor` is where textures stay pinned when they keep
    /// their scale. Overlays lying on the transformed brushes follow as their textures do.
    pub fn transform_selection(&mut self, matrix: DMat4, turn: bool, anchor: DVec3) -> Result<(), String> {
        if !transform::is_invertible(&matrix) {
            return Err("the transform would flatten the selection".to_owned());
        }
        let (solids, entities) = self.transform_targets()?;
        let lock = self.texture_lock(turn, anchor);
        let linear = transform::linear_part(&matrix);
        let riding = self.riding_overlay_edits(&[(solids.clone(), matrix, lock)]);
        let mut edits = vec![Edit::Transform { solids, matrix, lock }];
        for entity in entities {
            if let Some(origin) = entity.origin {
                edits.push(Edit::SetKey {
                    entity: entity.id,
                    key: "origin".to_owned(),
                    value: format_vec3(clean_vec3(matrix.transform_point3(origin))),
                });
            }
            let angles = entity.angles.or(entity.point.then_some(DVec3::ZERO));
            if let (true, Some(angles)) = (turn, angles) {
                if let Some(turned) = transform::transform_angles(angles, linear) {
                    if Some(turned) != entity.angles {
                        edits.push(Edit::SetKey {
                            entity: entity.id,
                            key: "angles".to_owned(),
                            value: format_vec3(turned),
                        });
                    }
                }
            }
        }
        edits.extend(self.selected_overlay_edits(&matrix));
        edits.extend(riding);
        self.apply_edit(&Edit::Batch(edits))?;
        if let Some(pivot) = self.overlay.pivot {
            self.overlay.pivot = Some(clean_vec3(matrix.transform_point3(pivot)));
        }
        Ok(())
    }

    /// Rotates the selection by angles about the X, Y and Z axes, in degrees.
    pub fn rotate_selection(&mut self, degrees: DVec3, pivot: Pivot) -> Result<(), String> {
        let center = self.pivot_point(pivot);
        self.transform_selection(around(center, rotation_xyz(degrees)), true, center)?;
        self.message = format!("Rotated selection by {}", format_vec3(degrees));
        Ok(())
    }

    /// Scales the selection by a factor along each axis. Negative factors mirror it.
    pub fn scale_selection(&mut self, factors: DVec3, pivot: Pivot) -> Result<(), String> {
        let center = self.pivot_point(pivot);
        let mirrored = factors.x * factors.y * factors.z < 0.0;
        self.transform_selection(around(center, DMat3::from_diagonal(factors)), mirrored, center)?;
        self.message = format!("Scaled selection by {}", format_vec3(factors));
        Ok(())
    }

    /// Mirrors the selection along a world axis (0 to 2) through its centre, like Hammer's
    /// Flip Objects.
    pub fn flip_selection(&mut self, axis: usize) -> Result<(), String> {
        let center = self.scene.selection_bounds().center();
        let mut factors = DVec3::ONE;
        factors[axis] = -1.0;
        self.transform_selection(around(center, DMat3::from_diagonal(factors)), true, center)?;
        self.message = format!("Flipped selection along {}", ["X", "Y", "Z"][axis]);
        Ok(())
    }

    /// Flips the selection horizontally or vertically in the 2D view used last.
    pub fn flip_in_view(&mut self, vertical: bool) -> Result<(), String> {
        let [horizontal, up, _] = self.views[self.active_view].axis.axes();
        self.flip_selection(if vertical { up } else { horizontal })
    }

    /// Moves the selection so its bounds' minimum corner lands on the grid, or each
    /// selected object on its own when `individually` is set, like Hammer's Snap Selected
    /// to Grid and Hammer++'s Snap Selected to Grid Individually.
    pub fn snap_selection_to_grid(&mut self, individually: bool) -> Result<(), String> {
        let grid = self.grid;
        let snap_offset = |anchor: DVec3| {
            DVec3::new(
                snap(anchor.x, grid) - anchor.x,
                snap(anchor.y, grid) - anchor.y,
                snap(anchor.z, grid) - anchor.z,
            )
        };
        let moves: Vec<(Owner, DVec3)> = if individually {
            self.selection
                .iter()
                .map(|owner| (*owner, snap_offset(self.owner_anchor(*owner))))
                .collect()
        } else {
            let offset = snap_offset(self.scene.selection_anchor());
            self.selection.iter().map(|owner| (*owner, offset)).collect()
        };
        let moved = self.move_owners(&moves)?;
        self.message = format!("Snapped {moved} objects to the {grid} grid");
        Ok(())
    }

    /// Lines up every selected object with one side of the selection's bounds: its
    /// minimum or maximum along a world axis, like Hammer's Align Objects.
    pub fn align_selection(&mut self, axis: usize, max: bool) -> Result<(), String> {
        let bounds = self.scene.selection_bounds();
        if bounds.is_empty() {
            return Err("nothing is selected".to_owned());
        }
        let moves: Vec<(Owner, DVec3)> = self
            .selection
            .iter()
            .map(|owner| {
                let own = self.owner_bounds(*owner);
                let mut offset = DVec3::ZERO;
                offset[axis] = if max {
                    bounds.max[axis] - own.max[axis]
                } else {
                    bounds.min[axis] - own.min[axis]
                };
                (*owner, offset)
            })
            .collect();
        let moved = self.move_owners(&moves)?;
        let side = if max { "maximum" } else { "minimum" };
        self.message = format!("Aligned {moved} objects to the {side} {}", ["X", "Y", "Z"][axis]);
        Ok(())
    }

    /// Lines up the selection with the left, right, top or bottom of its bounds in the 2D
    /// view used last.
    pub fn align_in_view(&mut self, vertical: bool, max: bool) -> Result<(), String> {
        let [horizontal, up, _] = self.views[self.active_view].axis.axes();
        self.align_selection(if vertical { up } else { horizontal }, max)
    }

    /// The bounds of one selected object.
    /// The point snapping moves one object by: a point entity's origin, or else the
    /// minimum corner of its bounds.
    pub fn owner_anchor(&self, owner: Owner) -> DVec3 {
        self.scene
            .meshes
            .iter()
            .find(|mesh| mesh.selects(owner))
            .and_then(|mesh| mesh.origin)
            .unwrap_or_else(|| self.owner_bounds(owner).min)
    }

    pub fn owner_bounds(&self, owner: Owner) -> Bounds {
        let mut bounds = Bounds::EMPTY;
        for mesh in self.scene.meshes.iter().filter(|mesh| mesh.selects(owner)) {
            bounds.add_bounds(&mesh.geometry_bounds);
        }
        bounds
    }

    /// Moves objects by their own offsets as one undo step, with texture lock, taking
    /// along the overlays that lie on each moved brush. Returns how many moved.
    fn move_owners(&mut self, moves: &[(Owner, DVec3)]) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let mut edits = Vec::new();
        let mut groups = Vec::new();
        let mut moved = 0;
        for (owner, offset) in moves.iter().filter(|(_, offset)| *offset != DVec3::ZERO) {
            moved += 1;
            let shift = DMat4::from_translation(*offset);
            match *owner {
                Owner::Solid(id) => {
                    groups.push((vec![id], shift, TextureLock::Full));
                    edits.push(Edit::Translate {
                        solids: vec![id],
                        offset: *offset,
                    });
                }
                Owner::Entity(id) => {
                    let entity = find_entity(vmf, id)?;
                    let solids: Vec<u32> = entity.solids().filter_map(Solid::id).collect();
                    if !solids.is_empty() {
                        groups.push((solids.clone(), shift, TextureLock::Full));
                        edits.push(Edit::Translate {
                            solids,
                            offset: *offset,
                        });
                    }
                    if let Some(origin) = entity.origin() {
                        edits.push(Edit::SetKey {
                            entity: id,
                            key: "origin".to_owned(),
                            value: format_vec3(clean_vec3(origin + *offset)),
                        });
                    }
                    edits.extend(overlay_edits(entity, &shift));
                }
            }
        }
        edits.extend(self.riding_overlay_edits(&groups));
        if !edits.is_empty() {
            self.apply_edit(&Edit::Batch(edits))?;
            if let (Some(pivot), [(_, offset), ..]) = (self.overlay.pivot, moves) {
                if moves.iter().all(|(_, other)| other == offset) {
                    self.overlay.pivot = Some(pivot + *offset);
                }
            }
        }
        Ok(moved)
    }

    /// Places the pivot that rotations turn around, or puts it back at the selection's
    /// centre with `None`.
    pub fn set_pivot(&mut self, pivot: Option<DVec3>) {
        self.overlay.pivot = pivot.map(clean_vec3);
    }
}

/// Rounds an angle in degrees to a step, for snapped rotations.
pub fn snap_angle(degrees: f64, step: f64) -> f64 {
    if step > 0.0 {
        clean((degrees / step).round() * step)
    } else {
        degrees
    }
}
