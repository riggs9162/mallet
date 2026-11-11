//! Placing and shaping overlays and decals: Hammer's Overlay and Decal tools.
//!
//! The Overlay tool puts an `info_overlay` with the current material on the brush face
//! under the cursor, lists the faces of that wall the picture reaches, and gives the
//! selected overlay a handle on each corner. The Decal tool puts an `infodecal` on the
//! face under the cursor. Overlays also ride along with the brushes they lie on when
//! those are moved with texture lock.

use std::collections::HashMap;

use glam::{DMat4, DVec2, DVec3};
use mallet_vmf::edit::Edit;
use mallet_vmf::geometry::Bounds;
use mallet_vmf::overlay::{
    follow_keys, Overlay, DECAL_CLASS, OVERLAY_CLASS, OVERLAY_TEXEL_SIZE, UPRIGHT_TEXTURE, UPRIGHT_TEXTURE_KEYS,
};
use mallet_vmf::transform::TextureLock;

use crate::editor::{picture_size, Editor, SelectMode};
use crate::face_pick::PickMode;
use crate::scene::{FaceHit, Owner};

/// The size of a new overlay whose material has no texture to measure, in world units.
const DEFAULT_OVERLAY_SIZE: f64 = 64.0;
/// The smallest width and height a corner drag leaves an overlay, in world units.
const MIN_OVERLAY_SIZE: f64 = 1.0;

/// How a dragged corner handle reshapes an overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerDrag {
    /// The two corners next to the dragged one follow it, so a rectangle stays one.
    Rectangle,
    /// Only the dragged corner moves, as in Hammer.
    Free,
}

impl Editor {
    /// Key edits that take overlays along with the brushes they lie on, the way texture
    /// lock takes those brushes' textures along. Each group is the brushes one transform
    /// moves, the transform, and how their textures follow it.
    ///
    /// An overlay follows when every brush face it lists is moved by the same group, so
    /// a poster that also lies on a brush left behind stays where it is. Selected
    /// overlays are left out, since they are transformed in their own right.
    pub(crate) fn riding_overlay_edits(&self, groups: &[(Vec<u32>, DMat4, TextureLock)]) -> Vec<Edit> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        if groups.iter().all(|(_, _, lock)| *lock == TextureLock::Off) {
            return Vec::new();
        }
        let riders: Vec<(u32, &mallet_vmf::Entity)> = document
            .vmf
            .entities()
            .filter(|entity| entity.classname().eq_ignore_ascii_case(OVERLAY_CLASS) && entity.get("sides").is_some())
            .filter_map(|entity| Some((entity.id()?, entity)))
            .filter(|(id, _)| !self.selection.contains(&Owner::Entity(*id)))
            .collect();
        if riders.is_empty() {
            return Vec::new();
        }
        let group_of: HashMap<u32, usize> = groups
            .iter()
            .enumerate()
            .flat_map(|(index, (solids, _, _))| solids.iter().map(move |solid| (*solid, index)))
            .collect();
        let mut side_group: HashMap<u32, Option<usize>> = HashMap::new();
        for solid in document.vmf.all_entities().flat_map(mallet_vmf::Entity::solids) {
            let group = solid.id().and_then(|id| group_of.get(&id).copied());
            side_group.extend(solid.sides().map(|side| (side.id, group)));
        }
        let mut edits = Vec::new();
        for (id, entity) in riders {
            let mut listed = entity
                .get("sides")
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|word| side_group.get(&word.parse::<u32>().ok()?).copied());
            let Some(Some(group)) = listed.next() else {
                continue;
            };
            if listed.any(|other| other != Some(group)) {
                continue;
            }
            let (_, matrix, lock) = &groups[group];
            edits.extend(
                follow_keys(entity, matrix, *lock)
                    .into_iter()
                    .map(|(key, value)| Edit::SetKey {
                        entity: id,
                        key: key.to_owned(),
                        value,
                    }),
            );
        }
        edits
    }

    /// The size a new overlay with `material` gets: its texture at the scale new brush
    /// faces use, so the picture keeps its shape.
    pub fn overlay_size(&mut self, material: &str) -> DVec2 {
        self.materials
            .as_mut()
            .and_then(|materials| materials.materials(&[material]).pop())
            .and_then(|material| picture_size(&material, OVERLAY_TEXEL_SIZE))
            .unwrap_or(DVec2::splat(DEFAULT_OVERLAY_SIZE))
    }

    /// Places an `info_overlay` with `material` on the brush face with side id `side`,
    /// centred on `point` pushed onto that face, and selects it. It is `size` units wide
    /// and high, or as large as its texture at 0.25 units per texel. Its `sides` list the
    /// face and every face in the same plane that the picture reaches. Undoable. Returns
    /// the new entity's id.
    pub fn create_overlay(
        &mut self,
        side: u32,
        point: DVec3,
        material: &str,
        size: Option<DVec2>,
    ) -> Result<u32, String> {
        let material = material.trim();
        if material.is_empty() {
            return Err("choose a material for the overlay first".to_owned());
        }
        let face = self
            .scene
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.faces)
            .find(|face| face.side == Some(side))
            .ok_or_else(|| format!("no brush face has side id {side}"))?;
        let (normal, anchor) = (face.normal, face.vertices[0]);
        let size = match size {
            Some(size) if size.min_element() > 0.0 => size,
            Some(_) => return Err("the overlay's size must be larger than zero".to_owned()),
            None => self.overlay_size(material),
        };
        let on_face = point - normal * normal.dot(point - anchor);
        let placed = Overlay::on_surface(on_face, normal, size);
        let sides = self.overlay_sides(&placed, side);
        let listed = sides.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
        let mut keys: Vec<(String, String)> = [("material", material), ("sides", listed.as_str())]
            .into_iter()
            .chain(UPRIGHT_TEXTURE_KEYS)
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        keys.extend(placed.keys().into_iter().map(|(key, value)| (key.to_owned(), value)));
        let id = self.create_entity(OVERLAY_CLASS, placed.origin, &keys)?;
        let count = sides.len();
        let faces = if count == 1 { "face" } else { "faces" };
        self.message = format!("Placed overlay {id} on {count} {faces}");
        Ok(id)
    }

    /// The faces a new overlay lists: `side` first, then the other faces in its plane
    /// that part of the picture lands on.
    fn overlay_sides(&self, placed: &Overlay, side: u32) -> Vec<u32> {
        let mut around = Bounds::EMPTY;
        for corner in placed.world_corners() {
            around.add_point(corner);
        }
        let mut sides = vec![side];
        for candidate in self.scene.faces_in_plane(side, &around) {
            let reached = self
                .scene
                .meshes
                .iter()
                .flat_map(|mesh| &mesh.faces)
                .find(|face| face.side == Some(candidate))
                .is_some_and(|face| {
                    let mut pieces = Vec::new();
                    placed.pieces_on(&UPRIGHT_TEXTURE, &face.vertices, face.normal, &mut pieces);
                    !pieces.is_empty()
                });
            if reached && !sides.contains(&candidate) {
                sides.push(candidate);
            }
        }
        sides
    }

    /// Places an `infodecal` with `material` at `point`, which should lie on a brush
    /// face, and selects it. Undoable. Returns the new entity's id.
    pub fn create_decal(&mut self, point: DVec3, material: &str) -> Result<u32, String> {
        let material = material.trim();
        if material.is_empty() {
            return Err("choose a material for the decal first".to_owned());
        }
        let point = (point * 100.0).round() / 100.0 + DVec3::ZERO;
        let keys = [("texture".to_owned(), material.to_owned())];
        let id = self.create_entity(DECAL_CLASS, point, &keys)?;
        self.message = format!("Placed decal {id}");
        Ok(id)
    }

    /// A click of the Overlay tool along a ray. An overlay under it is selected. On a
    /// bare brush face, Ctrl adds that face to the one selected overlay or takes it off
    /// it, and a plain click places a new overlay with the current material, centred on
    /// the click rounded to whole units.
    pub fn overlay_tool_click(&mut self, origin: DVec3, direction: DVec3, ctrl: bool) {
        if !ctrl && self.select_class_under(origin, direction, OVERLAY_CLASS) {
            return;
        }
        let Some((hit, normal)) = self.scene.surface_face(origin, direction) else {
            self.message = "Click on a brush face to place an overlay".to_owned();
            return;
        };
        let result = match (ctrl, self.handled_overlay()) {
            (true, Some((entity, _))) => self.toggle_overlay_face(entity, hit.side),
            (true, None) => Err("select one overlay to add faces to it or take them off".to_owned()),
            (false, _) => {
                let rounded = hit.point.round();
                let on_face = rounded - normal * normal.dot(rounded - hit.point);
                let material = self.material.clone();
                self.create_overlay(hit.side, on_face, &material, None).map(drop)
            }
        };
        if let Err(error) = result {
            self.message = error;
        }
    }

    /// A click of the Decal tool along a ray: selects a decal under it, or else places a
    /// new one with the current material where the ray meets a brush face.
    pub fn decal_tool_click(&mut self, origin: DVec3, direction: DVec3) {
        if self.select_class_under(origin, direction, DECAL_CLASS) {
            return;
        }
        let Some((FaceHit { point, .. }, _)) = self.scene.surface_face(origin, direction) else {
            self.message = "Click on a brush face to place a decal".to_owned();
            return;
        };
        let material = self.material.clone();
        if let Err(error) = self.create_decal(point, &material) {
            self.message = error;
        }
    }

    /// Selects the entity a ray hits first when it is of class `classname`. Returns
    /// whether it did.
    fn select_class_under(&mut self, origin: DVec3, direction: DVec3, classname: &str) -> bool {
        let Some(Owner::Entity(id)) = self.scene.pick_ray(origin, direction, &self.models, false) else {
            return false;
        };
        let matches = self
            .entity(id)
            .is_some_and(|entity| entity.classname().eq_ignore_ascii_case(classname));
        if matches {
            self.select([Owner::Entity(id)], SelectMode::Replace);
        }
        matches
    }

    /// The overlay the Overlay tool shows corner handles for: the one selected entity,
    /// when it is a placed overlay.
    pub fn handled_overlay(&self) -> Option<(u32, Overlay)> {
        let mut selected = self.selection.iter();
        let (Some(Owner::Entity(id)), None) = (selected.next(), selected.next()) else {
            return None;
        };
        Some((*id, Overlay::of(self.entity(*id)?)?))
    }

    /// Moves corner `corner` (0 to 3) of an overlay to `point`, pushed onto the
    /// overlay's plane and snapped to the grid along its axes. Steps with the same
    /// `group` undo together, as one drag.
    pub fn move_overlay_corner(
        &mut self,
        entity: u32,
        corner: usize,
        point: DVec3,
        drag: CornerDrag,
        group: u64,
    ) -> Result<(), String> {
        let grid = self.snap_step();
        let mut placed = self
            .entity(entity)
            .and_then(Overlay::of)
            .ok_or_else(|| format!("entity {entity} is not a placed overlay"))?;
        if corner >= 4 {
            return Err("an overlay has corners 0 to 3".to_owned());
        }
        let offset = point - placed.origin;
        let target = (DVec2::new(offset.dot(placed.u), offset.dot(placed.v)) / grid).round() * grid + DVec2::ZERO;
        let before = placed.corners;
        placed.corners[corner] = target;
        if drag == CornerDrag::Rectangle {
            placed.corners[corner ^ 1].x = target.x;
            placed.corners[3 - corner].y = target.y;
            let size = (placed.corners[corner] - placed.corners[(corner + 2) % 4]).abs();
            if size.min_element() < MIN_OVERLAY_SIZE {
                return Ok(());
            }
        }
        if placed.corners == before {
            return Ok(());
        }
        let edits = placed
            .keys()
            .into_iter()
            .filter(|(key, _)| key.starts_with("uv"))
            .map(|(key, value)| Edit::SetKey {
                entity,
                key: key.to_owned(),
                value,
            })
            .collect();
        self.apply_edit_grouped(&Edit::Batch(edits), group)?;
        let size = (placed.corners[2] - placed.corners[0]).abs();
        self.message = format!("Overlay {entity} is {}w {}h", size.x, size.y);
        Ok(())
    }

    /// Adds a brush face to an overlay's `sides`, or takes it off when it is listed.
    pub fn toggle_overlay_face(&mut self, entity: u32, side: u32) -> Result<(), String> {
        self.entity(entity)
            .filter(|target| target.classname().eq_ignore_ascii_case(OVERLAY_CLASS))
            .ok_or_else(|| format!("entity {entity} is not an overlay"))?;
        self.pick_side(&[entity], "sides", side, PickMode::Toggle).map(drop)
    }
}
