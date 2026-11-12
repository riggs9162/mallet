//! Hammer's displacement tools, as the Face Edit sheet's Displacement tab and the MCP
//! tools use them: Create and Destroy, Paint Geometry and Paint Alpha, Invert Alpha, Sew,
//! Subdivide, Noise and the elevation and scale attributes. They work on the faces the
//! texture tool has selected, or every face of the selected brushes. Every change
//! replaces the brushes involved through [`Edit::ReplaceSolids`], so it undoes exactly,
//! and a paint stroke is one undo step.

use std::collections::{BTreeSet, HashMap};

use glam::DVec3;
use mallet_vmf::displacement::{
    self, Channel, Dab, Displacement, Effect, Falloff, PaintAxis, Patch, MAX_POWER, MIN_POWER,
};
use mallet_vmf::edit::Edit;
use mallet_vmf::geometry::solid_faces;
use mallet_vmf::{Side, Solid};

use super::Editor;

/// The power Create uses until another is chosen, as in Hammer.
pub const DEFAULT_POWER: u32 = 3;
/// How far the paint brush reaches until another radius is chosen.
pub const DEFAULT_RADIUS: f64 = 64.0;
/// How far Paint Geometry moves vertices each time it is applied, until another
/// distance is chosen.
pub const DEFAULT_DISTANCE: f64 = 8.0;
/// How much Paint Alpha adds each time it is applied, until another value is chosen.
pub const DEFAULT_ALPHA: f64 = 64.0;

/// Which of the Displacement tab's tools clicks in the 3D view use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DisplacementMode {
    /// Clicks select faces, as the texture tool's do.
    #[default]
    Select,
    /// Hammer's Paint Geometry: dragging over the selected displacements raises them,
    /// and dragging with the right button lowers them.
    PaintGeometry,
    /// Hammer's Paint Alpha: dragging paints the selected displacements' alpha, and
    /// dragging with the right button takes it away.
    PaintAlpha,
    /// Hammer's Sculpt: a brush that is a circle on the screen pushes, carves, smooths or
    /// blends the selected displacements as it is dragged over them.
    Sculpt,
}

/// The direction Paint Geometry moves vertices in, as Hammer's dialog offers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AxisChoice {
    #[default]
    FaceNormal,
    X,
    Y,
    Z,
}

impl AxisChoice {
    pub const ALL: [AxisChoice; 4] = [AxisChoice::FaceNormal, AxisChoice::X, AxisChoice::Y, AxisChoice::Z];

    pub fn label(self) -> &'static str {
        match self {
            AxisChoice::FaceNormal => "Face Normal",
            AxisChoice::X => "X Axis",
            AxisChoice::Y => "Y Axis",
            AxisChoice::Z => "Z Axis",
        }
    }

    /// The axis as the paint brush takes it.
    pub fn paint_axis(self) -> PaintAxis {
        match self {
            AxisChoice::FaceNormal => PaintAxis::FaceNormal,
            AxisChoice::X => PaintAxis::Direction(DVec3::X),
            AxisChoice::Y => PaintAxis::Direction(DVec3::Y),
            AxisChoice::Z => PaintAxis::Direction(DVec3::Z),
        }
    }
}

/// The label Hammer's dialogs give a brush effect.
pub fn effect_label(effect: Effect) -> &'static str {
    match effect {
        Effect::Raise => "Raise/Lower",
        Effect::RaiseTo => "Raise To",
        Effect::Smooth => "Smooth",
    }
}

/// The Displacement tab's settings: its tool, the paint brush, Create's power and the
/// values of Noise.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplacementTool {
    pub mode: DisplacementMode,
    pub effect: Effect,
    pub axis: AxisChoice,
    pub falloff: Falloff,
    /// How far Paint Geometry moves vertices each time, or the height Raise To moves them to.
    pub distance: f64,
    /// How much alpha Paint Alpha adds each time, or the alpha Raise To sets.
    pub alpha: f64,
    pub radius: f64,
    /// Sew the edges of painted displacements to their selected neighbours after every
    /// step, as Hammer's Autosew does.
    pub autosew: bool,
    /// The power Create makes displacements with.
    pub power: u32,
    pub noise_min: f64,
    pub noise_max: f64,
    pub rockiness: f64,
    /// The Sculpt panel's settings.
    pub sculpt: super::SculptSettings,
}

impl Default for DisplacementTool {
    fn default() -> Self {
        Self {
            mode: DisplacementMode::Select,
            effect: Effect::Raise,
            axis: AxisChoice::FaceNormal,
            falloff: Falloff::Soft,
            distance: DEFAULT_DISTANCE,
            alpha: DEFAULT_ALPHA,
            radius: DEFAULT_RADIUS,
            autosew: true,
            power: DEFAULT_POWER,
            noise_min: 0.0,
            noise_max: 16.0,
            rockiness: 0.5,
            sculpt: super::SculptSettings::default(),
        }
    }
}

impl DisplacementTool {
    /// Whether clicks in the 3D view paint instead of selecting.
    pub fn painting(&self) -> bool {
        self.mode != DisplacementMode::Select
    }

    /// One application of the brush at `center`, as the current settings make it. With
    /// `reverse` (the right mouse button) raising lowers, and Raise To and Smooth do as
    /// they always do.
    pub fn dab(&self, center: DVec3, reverse: bool) -> Dab {
        let (channel, amount) = match self.mode {
            DisplacementMode::PaintAlpha => (Channel::Alpha, self.alpha),
            _ => (Channel::Geometry, self.distance),
        };
        let amount = if reverse && self.effect == Effect::Raise {
            -amount
        } else {
            amount
        };
        Dab {
            center,
            radius: self.radius.max(1.0),
            falloff: self.falloff,
            effect: self.effect,
            channel,
            amount,
            axis: self.axis.paint_axis(),
        }
    }
}

/// "1 displacement" or "3 displacements", for messages.
pub fn displacement_count(count: usize) -> String {
    if count == 1 {
        "1 displacement".to_owned()
    } else {
        format!("{count} displacements")
    }
}

/// A displacement face of the map, with the id of the brush it belongs to.
type Placed = (u32, Patch);

impl Editor {
    /// The faces the displacement tools work on: the faces the texture tool has selected,
    /// or every face of the selected brushes.
    pub fn displacement_sides(&self) -> BTreeSet<u32> {
        self.texture_targets()
    }

    /// The displacement faces among `sides`, ready to edit.
    pub fn displacement_patches(&self, sides: &BTreeSet<u32>) -> Vec<(u32, Patch)> {
        let mut patches = Vec::new();
        for solid in self.solids_where(|solid| {
            solid
                .sides()
                .any(|side| sides.contains(&side.id) && side.dispinfo().is_some())
        }) {
            let Some(id) = solid.id() else {
                continue;
            };
            let list: Vec<&Side> = solid.sides().collect();
            for face in solid_faces(&solid) {
                let Some(side) = list.get(face.side).filter(|side| sides.contains(&side.id)) else {
                    continue;
                };
                if let Some(patch) = Patch::new(side, &face.vertices, face.plane.normal) {
                    patches.push((id, patch));
                }
            }
        }
        patches
    }

    /// Hammer's Create: makes every four-sided face among `sides` a flat displacement of
    /// `power`, from 2 to 4. A face that already is one starts over. Returns how many
    /// were made.
    pub fn create_displacements(&mut self, sides: &BTreeSet<u32>, power: u32) -> Result<usize, String> {
        if sides.is_empty() {
            return Err("select the faces to make displacements of first".to_owned());
        }
        if !(MIN_POWER..=MAX_POWER).contains(&power) {
            return Err(format!("the power must be from {MIN_POWER} to {MAX_POWER}"));
        }
        let mut solids = self.solids_where(|solid| solid.sides().any(|side| sides.contains(&side.id)));
        let (mut made, mut skipped) = (0, 0);
        for solid in &mut solids {
            let polygons: HashMap<usize, (Vec<DVec3>, DVec3)> = solid_faces(solid)
                .into_iter()
                .map(|face| (face.side, (face.vertices, face.plane.normal)))
                .collect();
            for (index, side) in solid.sides_mut().enumerate() {
                if !sides.contains(&side.id) {
                    continue;
                }
                let created = polygons
                    .get(&index)
                    .and_then(|(polygon, normal)| Displacement::create(polygon, *normal, power));
                match created {
                    Some(displacement) => {
                        side.set_displacement(Some(&displacement));
                        made += 1;
                    }
                    None => skipped += 1,
                }
            }
        }
        if made == 0 {
            return Err("displacements can only be made on four-sided faces".to_owned());
        }
        solids.retain(|solid| {
            solid
                .sides()
                .any(|side| sides.contains(&side.id) && side.dispinfo().is_some())
        });
        self.replace_displaced(solids, None)?;
        let made_text = displacement_count(made);
        self.message = match skipped {
            0 => format!("Made {made_text} of power {power}"),
            1 => format!("Made {made_text} of power {power}; 1 face without four sides was left alone"),
            _ => format!("Made {made_text} of power {power}; {skipped} faces without four sides were left alone"),
        };
        Ok(made)
    }

    /// Hammer's Destroy: turns the displacements among `sides` back into plain faces.
    /// Returns how many there were.
    pub fn destroy_displacements(&mut self, sides: &BTreeSet<u32>) -> Result<usize, String> {
        let mut solids = self.solids_where(|solid| {
            solid
                .sides()
                .any(|side| sides.contains(&side.id) && side.dispinfo().is_some())
        });
        let mut count = 0;
        for solid in &mut solids {
            for side in solid.sides_mut().filter(|side| sides.contains(&side.id)) {
                if side.dispinfo().is_some() {
                    side.set_displacement(None);
                    count += 1;
                }
            }
        }
        if count == 0 {
            return Err("none of the selected faces is a displacement".to_owned());
        }
        self.replace_displaced(solids, None)?;
        self.message = format!("Destroyed {}", displacement_count(count));
        Ok(count)
    }

    /// Where a ray meets the displacements among `sides` first, and the surface's
    /// normal there, for placing the paint brush.
    pub fn displacement_hit(&self, sides: &BTreeSet<u32>, origin: DVec3, direction: DVec3) -> Option<(DVec3, DVec3)> {
        self.scene
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.faces)
            .filter(|face| face.side.is_some_and(|side| sides.contains(&side)))
            .filter_map(|face| {
                let surface = face.displacement.as_ref()?;
                let distance = surface.ray_hit(origin, direction)?;
                let point = origin + direction * distance;
                let normal = surface
                    .nearest_vertex(point)
                    .map_or(surface.normal, |vertex| surface.normals[vertex]);
                Some((distance, point, normal))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, point, normal)| (point, normal))
    }

    /// Applies one dab of the paint brush to the displacements among `sides`, sewing
    /// them to each other afterwards with `autosew`. Dabs with the same `stroke` undo
    /// together. Returns how many displacements changed.
    pub fn paint_displacements(
        &mut self,
        sides: &BTreeSet<u32>,
        dab: &Dab,
        autosew: bool,
        stroke: u64,
    ) -> Result<usize, String> {
        let (owners, mut patches) = split(self.displacement_patches(sides));
        if patches.is_empty() {
            return Err("select the displacements to paint first".to_owned());
        }
        let mut changed = displacement::paint(&mut patches, dab);
        if autosew && !changed.is_empty() {
            changed.extend(displacement::sew(&mut patches));
        }
        self.store_patches(&owners, &patches, &changed, Some(stroke))
    }

    /// Hammer's Sew: joins the shared edges and corners of the displacements among
    /// `sides`. Returns how many displacements changed.
    pub fn sew_displacements(&mut self, sides: &BTreeSet<u32>) -> Result<usize, String> {
        self.edit_patches(sides, |patches| Ok(displacement::sew(patches)))
            .inspect(|count| self.message = format!("Sewed {}", displacement_count(*count)))
    }

    /// Hammer's Subdivide: rounds the displacements among `sides` off as one smooth
    /// surface. Returns how many changed.
    pub fn subdivide_displacements(&mut self, sides: &BTreeSet<u32>) -> Result<usize, String> {
        self.edit_patches(sides, displacement::subdivide)
            .inspect(|count| self.message = format!("Subdivided {}", displacement_count(*count)))
    }

    /// Hammer's Noise: raises every vertex of the displacements among `sides` by a
    /// smooth random amount from `min` to `max`. Returns how many changed.
    pub fn displacement_noise(
        &mut self,
        sides: &BTreeSet<u32>,
        min: f64,
        max: f64,
        rockiness: f64,
    ) -> Result<usize, String> {
        let seed = (crate::scene::next_generation() & u64::from(u32::MAX)) as u32;
        self.edit_patches(sides, |patches| {
            Ok(displacement::noise(patches, min, max, rockiness, seed))
        })
        .inspect(|count| self.message = format!("Added noise to {}", displacement_count(*count)))
    }

    /// Hammer's Invert Alpha on the displacements among `sides`. Returns how many changed.
    pub fn invert_displacement_alpha(&mut self, sides: &BTreeSet<u32>) -> Result<usize, String> {
        self.edit_patches(sides, |patches| {
            for patch in patches.iter_mut() {
                patch.displacement.invert_alpha();
                patch.refresh();
            }
            Ok((0..patches.len()).collect())
        })
        .inspect(|count| self.message = format!("Inverted the alpha of {}", displacement_count(*count)))
    }

    /// Sets how far the displacements among `sides` sit off their faces, as Hammer's
    /// Elevation attribute does. Returns how many changed.
    pub fn set_displacement_elevation(&mut self, sides: &BTreeSet<u32>, elevation: f64) -> Result<usize, String> {
        self.edit_patches(sides, |patches| {
            for patch in patches.iter_mut() {
                patch.displacement.elevation = elevation;
                patch.refresh();
            }
            Ok((0..patches.len()).collect())
        })
    }

    /// Scales how far every vertex of the displacements among `sides` is moved, as
    /// Hammer's Scale attribute does. Returns how many changed.
    pub fn scale_displacements(&mut self, sides: &BTreeSet<u32>, factor: f64) -> Result<usize, String> {
        if !factor.is_finite() {
            return Err("the scale must be a number".to_owned());
        }
        self.edit_patches(sides, |patches| {
            for patch in patches.iter_mut() {
                for vertex in 0..patch.displacement.distances.len() {
                    let vector = patch.displacement.vector(vertex) * factor;
                    patch.displacement.set_vector(vertex, vector, patch.surface.normal);
                }
                patch.refresh();
            }
            Ok((0..patches.len()).collect())
        })
    }

    /// Runs `change` on the displacements among `sides` and stores the patches it says
    /// changed as one undo step.
    fn edit_patches(
        &mut self,
        sides: &BTreeSet<u32>,
        change: impl FnOnce(&mut [Patch]) -> Result<Vec<usize>, String>,
    ) -> Result<usize, String> {
        let (owners, mut patches) = split(self.displacement_patches(sides));
        if patches.is_empty() {
            return Err("select some displacements first".to_owned());
        }
        let changed = change(&mut patches)?;
        self.store_patches(&owners, &patches, &changed, None)
    }

    /// Writes the changed patches into their brushes as one edit, or as a step of the
    /// undo group `group`. Returns how many patches changed.
    pub(super) fn store_patches(
        &mut self,
        owners: &[u32],
        patches: &[Patch],
        changed: &[usize],
        group: Option<u64>,
    ) -> Result<usize, String> {
        let mut by_side: HashMap<u32, &Displacement> = HashMap::new();
        let mut solids_changed: BTreeSet<u32> = BTreeSet::new();
        for &index in changed {
            by_side.insert(patches[index].side, &patches[index].displacement);
            solids_changed.insert(owners[index]);
        }
        if by_side.is_empty() {
            return Ok(0);
        }
        let mut solids = self.solids_where(|solid| solid.id().is_some_and(|id| solids_changed.contains(&id)));
        for solid in &mut solids {
            for side in solid.sides_mut() {
                if let Some(displacement) = by_side.get(&side.id) {
                    side.set_displacement(Some(displacement));
                }
            }
        }
        self.replace_displaced(solids, group)?;
        Ok(by_side.len())
    }

    /// Replaces brushes whose displacements changed, updating only their meshes when
    /// their faces stay the same.
    fn replace_displaced(&mut self, solids: Vec<Solid>, group: Option<u64>) -> Result<(), String> {
        let edit = Edit::ReplaceSolids(solids.clone());
        let document = self.document.as_mut().ok_or("no map is open")?;
        match group {
            Some(group) => document.history.apply_grouped(&mut document.vmf, &edit, group),
            None => document.history.apply(&mut document.vmf, &edit),
        }
        .map_err(|error| error.to_string())?;
        document.dirty = true;
        if !self.scene.retexture(&solids) {
            self.rebuild_scene();
        }
        self.refresh_face_marks();
        Ok(())
    }
}

/// Splits placed patches into their brushes' ids and the patches, in the same order.
pub(super) fn split(placed: Vec<Placed>) -> (Vec<u32>, Vec<Patch>) {
    placed.into_iter().unzip()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::SelectMode;
    use crate::scene::Owner;

    /// A block with a displacement on its top face raised 48 units above it.
    fn raised_displacement() -> (Editor, u32) {
        let mut editor = Editor::default();
        editor.new_map();
        let solid = editor
            .create_block(DVec3::ZERO, DVec3::new(128.0, 128.0, 64.0))
            .unwrap();
        let top = editor
            .scene
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.faces)
            .find(|face| face.solid == Some(solid) && face.normal.z > 0.5)
            .and_then(|face| face.side)
            .unwrap();
        let sides = BTreeSet::from([top]);
        editor.create_displacements(&sides, 2).unwrap();
        editor.set_displacement_elevation(&sides, 48.0).unwrap();
        (editor, solid)
    }

    #[test]
    fn a_displacement_is_selected_and_moved_by_its_brush_as_in_hammer() {
        let (mut editor, solid) = raised_displacement();
        editor.select([Owner::Solid(solid)], SelectMode::Replace);
        let mesh = editor.scene.meshes.iter().find(|mesh| mesh.solid == Some(solid)).unwrap();
        assert!(mesh.bounds.max.z > 111.9, "the raised surface is drawn above the brush");
        let selected = editor.scene.selection_bounds();
        assert_eq!((selected.min, selected.max), (DVec3::ZERO, DVec3::new(128.0, 128.0, 64.0)));
        assert_eq!(editor.scene.selection_anchor(), DVec3::ZERO);

        editor.move_selection(DVec3::new(64.0, 0.0, 0.0)).unwrap();
        let moved = editor.scene.selection_bounds();
        assert_eq!((moved.min, moved.max), (DVec3::new(64.0, 0.0, 0.0), DVec3::new(192.0, 128.0, 64.0)));
    }
}
