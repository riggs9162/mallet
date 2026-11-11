//! The texture tool's side of the editor: which brush faces are selected, and changing
//! their material and alignment. Every change replaces the brushes involved through
//! [`Edit::ReplaceSolids`], so it undoes exactly; dragging a value groups its steps into
//! one undo step.

use std::collections::hash_map::DefaultHasher;
use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};

use glam::DVec3;
use mallet_vmf::edit::Edit;
use mallet_vmf::geometry::solid_faces;
use mallet_vmf::texture::{Hotspot, Justify};
use mallet_vmf::{Side, Solid};

use eframe::egui;

use crate::editor::{Editor, SelectMode, Tool};
use crate::scene::{FaceHit, Owner};

/// Texture size assumed when a material has no texture, as Hammer does.
const DEFAULT_TEXTURE_SIZE: f64 = 64.0;

/// Values the face panel sets on every selected face; `None` leaves a value alone.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FaceValues {
    pub scale: [Option<f64>; 2],
    pub shift: [Option<f64>; 2],
    pub rotation: Option<f64>,
    pub lightmap_scale: Option<u32>,
    /// Texture axis directions, the UV vectors of Face Edit.
    pub axes: [Option<DVec3>; 2],
    pub smoothing_groups: Option<u32>,
}

/// What a left click on a face does with the texture tool, like Hammer's Mode button.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextureMode {
    /// Select the face and pick up its material and alignment.
    #[default]
    LiftSelect,
    /// Pick up the material and alignment without changing the selection.
    Lift,
    /// Select the face only.
    Select,
    /// Put the current material on the face.
    ApplyTexture,
    /// Put the current material and the picked-up alignment on the face.
    ApplyAll,
    /// Project the texture onto the face from the 3D camera, like a slide projector.
    AlignToView,
}

impl TextureMode {
    pub const ALL: [TextureMode; 6] = [
        TextureMode::LiftSelect,
        TextureMode::Lift,
        TextureMode::Select,
        TextureMode::ApplyTexture,
        TextureMode::ApplyAll,
        TextureMode::AlignToView,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TextureMode::LiftSelect => "Lift+Select",
            TextureMode::Lift => "Lift",
            TextureMode::Select => "Select",
            TextureMode::ApplyTexture => "Apply (texture only)",
            TextureMode::ApplyAll => "Apply (texture + values)",
            TextureMode::AlignToView => "Align to View",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            TextureMode::LiftSelect => "Clicking a face selects it and makes its texture the current texture.",
            TextureMode::Lift => "Clicking a face makes its texture current without selecting it.",
            TextureMode::Select => "Clicking a face selects it without changing the current texture.",
            TextureMode::ApplyTexture => "Clicking a face puts the current texture on it, keeping its alignment.",
            TextureMode::ApplyAll => "Clicking a face puts the current texture and the lifted alignment on it.",
            TextureMode::AlignToView => {
                "Clicking a face projects its texture from the camera, like a slide projector. Good for cliffs."
            }
        }
    }
}

/// What Replace Textures does with the materials it finds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReplaceAction {
    /// Materials named exactly like the search become the replacement.
    #[default]
    Exact,
    /// Materials containing the search become the replacement.
    Partial,
    /// The part of each material that matches the search becomes the replacement.
    Substitute,
}

/// Options of Replace Textures.
#[derive(Debug, Clone, Default)]
pub struct ReplaceOptions {
    pub action: ReplaceAction,
    /// Only these faces; everything when `None`.
    pub within: Option<BTreeSet<u32>>,
    /// Select the faces found instead of changing them.
    pub mark_only: bool,
    /// Change the scale so the new texture covers the same area as the old one.
    pub rescale: bool,
}

/// Several face changes made as one undo step, as the `set_faces` MCP tool does.
#[derive(Debug, Clone, Default)]
pub struct FaceChange {
    pub material: Option<String>,
    /// Align to the face (`true`) or the world (`false`).
    pub align: Option<bool>,
    pub values: FaceValues,
    pub justify: Option<Justify>,
    pub as_one: bool,
    /// With [`Justify::Fit`] on hotspot textures, use only the alternate rectangles.
    pub alternates: bool,
}

/// One face, with what the face panel and MCP tools show about it.
#[derive(Debug, Clone)]
pub struct FaceInfo {
    pub solid: u32,
    pub side: Side,
    /// The face polygon, empty when the side has no area.
    pub polygon: Vec<DVec3>,
}

impl FaceInfo {
    pub fn normal(&self) -> DVec3 {
        mallet_vmf::texture::side_normal(&self.side)
    }

    pub fn center(&self) -> DVec3 {
        self.polygon.iter().copied().sum::<DVec3>() / self.polygon.len().max(1) as f64
    }
}

impl Editor {
    /// Every face of the brushes with these ids, with its polygon.
    pub fn brush_faces(&self, solids: &[u32]) -> Vec<FaceInfo> {
        self.solids_where(|solid| solid.id().is_some_and(|id| solids.contains(&id)))
            .into_iter()
            .flat_map(|solid| faces_of(&solid))
            .collect()
    }

    /// The faces with these side ids, with their polygons.
    pub fn faces(&self, sides: &BTreeSet<u32>) -> Vec<FaceInfo> {
        self.solids_where(|solid| solid.sides().any(|side| sides.contains(&side.id)))
            .into_iter()
            .flat_map(|solid| faces_of(&solid))
            .filter(|face| sides.contains(&face.side.id))
            .collect()
    }

    /// Clones of the brushes that pass `wanted`, in every entity.
    fn solids_where(&self, wanted: impl Fn(&Solid) -> bool) -> Vec<Solid> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        document
            .vmf
            .all_entities()
            .flat_map(|entity| entity.solids())
            .filter(|solid| wanted(solid))
            .cloned()
            .collect()
    }

    /// Changes which faces are selected.
    pub fn select_faces(&mut self, sides: impl IntoIterator<Item = u32>, mode: SelectMode) {
        let sides: Vec<u32> = sides.into_iter().collect();
        match mode {
            SelectMode::Replace => self.face_selection = sides.into_iter().collect(),
            SelectMode::Add => self.face_selection.extend(sides),
            SelectMode::Remove => {
                for side in sides {
                    self.face_selection.remove(&side);
                }
            }
            SelectMode::Toggle => {
                for side in sides {
                    if !self.face_selection.remove(&side) {
                        self.face_selection.insert(side);
                    }
                }
            }
        }
        self.refresh_face_marks();
    }

    pub fn clear_face_selection(&mut self) {
        if !self.face_selection.is_empty() {
            self.face_selection.clear();
            self.refresh_face_marks();
        }
    }

    /// Every face of these brushes and of the brushes these entities own.
    pub fn owner_sides<'a>(&self, owners: impl IntoIterator<Item = &'a Owner>) -> Vec<u32> {
        let solids: Vec<u32> = owners
            .into_iter()
            .flat_map(|owner| match owner {
                Owner::Solid(id) => vec![*id],
                Owner::Entity(id) => self
                    .entity(*id)
                    .map(|entity| entity.solids().filter_map(Solid::id).collect())
                    .unwrap_or_default(),
            })
            .collect();
        if solids.is_empty() {
            return Vec::new();
        }
        self.brush_faces(&solids).iter().map(|face| face.side.id).collect()
    }

    /// The faces texture changes go to: the faces the texture tool selected, or else every
    /// face of the selected brushes, so a texture picked with brushes selected covers them.
    pub fn texture_targets(&self) -> BTreeSet<u32> {
        if !self.face_selection.is_empty() {
            return self.face_selection.clone();
        }
        self.owner_sides(&self.selection).into_iter().collect()
    }

    /// The texture tool's take on selecting brushes: every face of each brush or entity is
    /// selected, added or removed instead. Toggling takes a brush's faces away when all of
    /// them are selected and selects them all otherwise.
    pub(crate) fn select_owner_faces(&mut self, owners: &[Owner], mode: SelectMode) {
        if mode != SelectMode::Toggle {
            let sides = self.owner_sides(owners);
            return self.select_faces(sides, mode);
        }
        for owner in owners {
            let sides = self.owner_sides([owner]);
            let all = sides.iter().all(|side| self.face_selection.contains(side));
            for side in sides {
                if all {
                    self.face_selection.remove(&side);
                } else {
                    self.face_selection.insert(side);
                }
            }
        }
        self.refresh_face_marks();
    }

    /// Switches tools. Like Hammer, the texture tool trades the selected brushes for all of
    /// their faces, and leaving it lets go of the faces; other tools drop pending previews.
    pub fn set_tool(&mut self, tool: Tool) {
        if self.tool == tool {
            return;
        }
        if self.tool == Tool::Texture {
            self.clear_face_selection();
        }
        let sides = (tool == Tool::Texture).then(|| self.owner_sides(&self.selection));
        if sides.is_some() {
            self.clear_selection();
        }
        self.tool = tool;
        self.overlay.block = None;
        self.overlay.entity = None;
        if let Some(sides) = sides {
            self.select_faces(sides, SelectMode::Replace);
        }
        self.clear_clip_points();
    }

    /// Remembers a material as used, for the texture browser's recent list.
    pub fn remember_material(&mut self, material: &str) {
        self.recent_materials
            .retain(|recent| !recent.eq_ignore_ascii_case(material));
        self.recent_materials.insert(0, material.to_owned());
        self.recent_materials
            .truncate(self.config.editing.recent_texture_count());
    }

    /// What a texture tool click on a face does: Alt picks its material up, Shift takes
    /// the whole brush, Ctrl adds or removes, and a plain click selects just that face.
    pub fn texture_click(&mut self, hit: Option<FaceHit>, modifiers: egui::Modifiers) {
        let Some(hit) = hit else {
            if !modifiers.ctrl && matches!(self.texture_mode, TextureMode::LiftSelect | TextureMode::Select) {
                self.clear_face_selection();
            }
            return;
        };
        let mode = if modifiers.alt {
            TextureMode::Lift
        } else {
            self.texture_mode
        };
        match mode {
            TextureMode::Lift => return self.lift(hit.side),
            TextureMode::ApplyTexture => return self.texture_apply_click(Some(hit), false),
            TextureMode::ApplyAll => return self.texture_apply_click(Some(hit), true),
            TextureMode::AlignToView => return self.align_to_view(hit.side),
            TextureMode::LiftSelect => self.lift(hit.side),
            TextureMode::Select => {}
        }
        if modifiers.shift {
            let sides: Vec<u32> = self.brush_faces(&[hit.solid]).iter().map(|face| face.side.id).collect();
            let mode = if modifiers.ctrl {
                SelectMode::Add
            } else {
                SelectMode::Replace
            };
            self.select_faces(sides, mode);
        } else if modifiers.ctrl {
            self.select_faces([hit.side], SelectMode::Toggle);
        } else {
            self.select_faces([hit.side], SelectMode::Replace);
        }
    }

    /// A texture tool right-click: puts the current material on the face under the
    /// cursor, and with `with_values` (Alt) also the picked-up alignment.
    pub fn texture_apply_click(&mut self, hit: Option<FaceHit>, with_values: bool) {
        let Some(hit) = hit else { return };
        let material = self.material.clone();
        let lifted = self.lifted.clone().filter(|_| with_values);
        let sides = BTreeSet::from([hit.side]);
        let result = match lifted {
            Some(lifted) => self
                .edit_sides(&sides, None, |side, _| {
                    side.material.clone_from(&material);
                    side.uaxis = lifted.uaxis;
                    side.vaxis = lifted.vaxis;
                    side.rotation = lifted.rotation;
                    side.lightmap_scale = lifted.lightmap_scale;
                })
                .inspect(|_| self.remember_material(&material)),
            None => self.apply_material(&sides, &material),
        };
        if let Err(error) = result {
            self.message = error;
        }
    }

    /// A texture tool click in a 2D view: every face of the brush or entity picked.
    pub fn texture_click_2d(&mut self, hit: Option<Owner>, modifiers: egui::Modifiers) {
        let solids: Vec<u32> = match hit {
            Some(Owner::Solid(id)) => vec![id],
            Some(Owner::Entity(id)) => self
                .entity(id)
                .map(|entity| entity.solids().filter_map(Solid::id).collect())
                .unwrap_or_default(),
            None => {
                if !modifiers.ctrl {
                    self.clear_face_selection();
                }
                return;
            }
        };
        let sides: Vec<u32> = self.brush_faces(&solids).iter().map(|face| face.side.id).collect();
        let mode = if modifiers.ctrl {
            SelectMode::Toggle
        } else {
            SelectMode::Replace
        };
        self.select_faces(sides, mode);
    }

    /// The width and height of a material's texture in texels.
    pub fn texture_size(&mut self, material: &str) -> [f64; 2] {
        self.materials
            .as_mut()
            .and_then(|materials| materials.size(material))
            .map_or([DEFAULT_TEXTURE_SIZE; 2], |size| size.map(f64::from))
    }

    /// Changes the sides with these ids through `change`, which gets each side and its
    /// polygon. With a `group`, consecutive changes in the same group undo as one.
    pub fn edit_sides(
        &mut self,
        sides: &BTreeSet<u32>,
        group: Option<&str>,
        mut change: impl FnMut(&mut Side, &[DVec3]),
    ) -> Result<usize, String> {
        if sides.is_empty() {
            return Err("select some faces first".to_owned());
        }
        let mut solids = self.solids_where(|solid| solid.sides().any(|side| sides.contains(&side.id)));
        let mut count = 0;
        for solid in &mut solids {
            let polygons: HashMap<u32, Vec<DVec3>> = faces_of(solid)
                .into_iter()
                .map(|face| (face.side.id, face.polygon))
                .collect();
            for side in solid.sides_mut().filter(|side| sides.contains(&side.id)) {
                let polygon = polygons.get(&side.id).map(Vec::as_slice).unwrap_or(&[]);
                change(side, polygon);
                count += 1;
            }
        }
        if count == 0 {
            return Err("none of those faces exist".to_owned());
        }
        let changed = solids.clone();
        let edit = Edit::ReplaceSolids(solids);
        let document = self.document.as_mut().ok_or("no map is open")?;
        let result = match group {
            Some(group) => {
                let mut hasher = DefaultHasher::new();
                (group, sides).hash(&mut hasher);
                document
                    .history
                    .apply_grouped(&mut document.vmf, &edit, hasher.finish())
            }
            None => document.history.apply(&mut document.vmf, &edit),
        };
        result.map_err(|error| error.to_string())?;
        document.dirty = true;
        // Only materials and alignment changed, so only these brushes need new meshes.
        if !self.scene.retexture(&changed) {
            self.rebuild_scene();
        }
        self.refresh_face_marks();
        Ok(count)
    }

    /// Puts a material on faces, keeping their alignment.
    pub fn apply_material(&mut self, sides: &BTreeSet<u32>, material: &str) -> Result<usize, String> {
        let material = material.trim().to_owned();
        if material.is_empty() {
            return Err("choose a material first".to_owned());
        }
        let count = self.edit_sides(sides, None, |side, _| side.material.clone_from(&material))?;
        self.remember_material(&material);
        self.message = format!("Applied {material} to {count} faces");
        Ok(count)
    }

    /// Sets scale, shift, rotation or lightmap scale on faces. `group` lets a drag make a
    /// single undo step.
    pub fn set_face_values(
        &mut self,
        sides: &BTreeSet<u32>,
        values: FaceValues,
        group: Option<&str>,
    ) -> Result<usize, String> {
        self.edit_sides(sides, group, |side, _| set_values(side, &values))
    }

    /// Material, alignment, values and justify in one undo step, in that order.
    pub fn change_faces(&mut self, sides: &BTreeSet<u32>, change: &FaceChange) -> Result<usize, String> {
        let mut size_of = HashMap::new();
        let faces = self.faces(sides);
        for material in faces
            .iter()
            .map(|face| face.side.material.clone())
            .chain(change.material.clone())
        {
            if let Entry::Vacant(entry) = size_of.entry(material) {
                let size = self.texture_size(entry.key());
                entry.insert(size);
            }
        }
        let all_points: Vec<DVec3> = faces.iter().flat_map(|face| face.polygon.iter().copied()).collect();
        let hotspots = match change.justify {
            Some(Justify::Fit) => self.hotspots_of(size_of.keys().cloned().collect::<Vec<_>>()),
            _ => HashMap::new(),
        };
        let seed = random_seed();
        let values = change.values;
        let count = self.edit_sides(sides, None, |side, polygon| {
            if let Some(material) = &change.material {
                side.material.clone_from(material);
            }
            if let Some(to_face) = change.align {
                side.align(to_face);
            }
            set_values(side, &values);
            if let Some(how) = change.justify {
                let size = size_of
                    .get(&side.material)
                    .copied()
                    .unwrap_or([DEFAULT_TEXTURE_SIZE; 2]);
                let points = if change.as_one { all_points.as_slice() } else { polygon };
                let rects = hotspots.get(&side.material).map(Vec::as_slice).unwrap_or(&[]);
                if how != Justify::Fit
                    || rects.is_empty()
                    || !side.hotspot(polygon, rects, change.alternates, face_random(seed, side.id))
                {
                    side.justify(how, points, size);
                }
            }
        })?;
        if let Some(material) = &change.material {
            self.remember_material(material);
        }
        Ok(count)
    }

    /// Projects a face's texture from the 3D camera: its axes become the camera's right
    /// and down directions.
    fn align_to_view(&mut self, side: u32) {
        let forward = self.camera.forward();
        let right = forward.cross(DVec3::Z).normalize_or(DVec3::X);
        let down = forward.cross(right).normalize_or(DVec3::NEG_Z);
        let result = self.edit_sides(&BTreeSet::from([side]), None, |side, _| {
            side.uaxis.axis = right;
            side.vaxis.axis = down;
            side.rotation = Some(0.0);
        });
        if let Err(error) = result {
            self.message = error;
        }
    }

    /// Shifts each face's texture by a random whole number of texels along one axis,
    /// so repeated faces such as stair steps do not all look the same.
    pub fn random_shift(&mut self, sides: &BTreeSet<u32>, horizontal: bool) -> Result<usize, String> {
        let faces = self.faces(sides);
        let mut sizes = HashMap::new();
        for face in &faces {
            if !sizes.contains_key(&face.side.material) {
                let size = self.texture_size(&face.side.material);
                sizes.insert(face.side.material.clone(), size);
            }
        }
        let seed = random_seed();
        self.edit_sides(sides, None, |side, _| {
            let [width, height] = sizes.get(&side.material).copied().unwrap_or([DEFAULT_TEXTURE_SIZE; 2]);
            let range = if horizontal { width } else { height };
            let random = face_random(seed ^ u64::from(horizontal), side.id);
            let shift = (random % range.max(1.0) as u64) as f64;
            if horizontal {
                side.uaxis.offset = shift;
            } else {
                side.vaxis.offset = shift;
            }
        })
    }

    /// Selects every face that uses a material, like Hammer's Mark button.
    pub fn mark_material(&mut self, material: &str) -> usize {
        let wanted = material.trim().replace('\\', "/");
        let sides: Vec<u32> = self
            .solids_where(|_| true)
            .iter()
            .flat_map(|solid| solid.sides())
            .filter(|side| side.material.replace('\\', "/").eq_ignore_ascii_case(&wanted))
            .map(|side| side.id)
            .collect();
        let count = sides.len();
        self.select_faces(sides, SelectMode::Replace);
        self.message = format!("Marked {count} faces using {wanted}");
        count
    }

    /// Remembers a face's material and alignment, for applying elsewhere.
    fn lift(&mut self, side: u32) {
        if let Some(face) = self.faces(&BTreeSet::from([side])).pop() {
            self.material = face.side.material.clone();
            self.message = format!("Picked up {}", face.side.material);
            self.lifted = Some(face.side);
        }
    }

    /// Lines textures up on their faces. With `as_one`, the faces share one extent, so a
    /// texture runs across them as if they were one face.
    ///
    /// Fit on a hotspot texture works as in Hammer++: each face gets the rectangle of the
    /// texture that suits its size best, picked at random among equally good ones, so
    /// pressing Fit again can give another result. `alternates` limits the choice to the
    /// rectangles marked as alternates, as holding Alt does in Hammer++.
    pub fn justify_faces(
        &mut self,
        sides: &BTreeSet<u32>,
        how: Justify,
        as_one: bool,
        alternates: bool,
    ) -> Result<usize, String> {
        let faces = self.faces(sides);
        let mut sizes = HashMap::new();
        for face in &faces {
            if !sizes.contains_key(&face.side.material) {
                let size = self.texture_size(&face.side.material);
                sizes.insert(face.side.material.clone(), size);
            }
        }
        let hotspots = if how == Justify::Fit {
            self.hotspots_of(sizes.keys().cloned().collect::<Vec<_>>())
        } else {
            HashMap::new()
        };
        let seed = random_seed();
        let mut hotspotted = 0;
        let all_points: Vec<DVec3> = faces.iter().flat_map(|face| face.polygon.iter().copied()).collect();
        let count = self.edit_sides(sides, None, |side, polygon| {
            if let Some(rects) = hotspots.get(&side.material) {
                if side.hotspot(polygon, rects, alternates, face_random(seed, side.id)) {
                    hotspotted += 1;
                    return;
                }
            }
            let size = sizes.get(&side.material).copied().unwrap_or([DEFAULT_TEXTURE_SIZE; 2]);
            let points = if as_one { all_points.as_slice() } else { polygon };
            side.justify(how, points, size);
        })?;
        if hotspotted > 0 {
            self.message = format!("Hotspotted {hotspotted} of {count} faces");
        }
        Ok(count)
    }

    /// The hotspot rectangles of a material, from the `.rect` file its `%rectanglemap`
    /// names; empty for ordinary materials.
    pub fn hotspots(&mut self, material: &str) -> Vec<Hotspot> {
        let Some(text) = self
            .materials
            .as_mut()
            .and_then(|materials| materials.rectangle_file(material))
        else {
            return Vec::new();
        };
        Hotspot::parse_rect_file(&text).unwrap_or_default()
    }

    /// The hotspot rectangles of every material in `materials` that has any.
    fn hotspots_of(&mut self, materials: Vec<String>) -> HashMap<String, Vec<Hotspot>> {
        materials
            .into_iter()
            .filter_map(|material| {
                let rects = self.hotspots(&material);
                (!rects.is_empty()).then_some((material, rects))
            })
            .collect()
    }

    /// Aligns textures to the world or to their faces.
    pub fn align_faces(&mut self, sides: &BTreeSet<u32>, to_face: bool) -> Result<usize, String> {
        self.edit_sides(sides, None, |side, _| side.align(to_face))
    }

    /// Replaces a material everywhere in the map, or only on `within` faces. Returns how
    /// many faces changed.
    pub fn replace_material(
        &mut self,
        find: &str,
        replace: &str,
        within: Option<&BTreeSet<u32>>,
    ) -> Result<usize, String> {
        let options = ReplaceOptions {
            within: within.cloned(),
            ..ReplaceOptions::default()
        };
        self.replace_materials(find, replace, &options)
    }

    /// Hammer's Replace Textures dialog. Returns how many faces matched.
    pub fn replace_materials(&mut self, find: &str, replace: &str, options: &ReplaceOptions) -> Result<usize, String> {
        let find = find.trim().replace('\\', "/").to_ascii_lowercase();
        if find.is_empty() {
            return Err("give the material to find".to_owned());
        }
        let matches = |material: &str| {
            let material = material.replace('\\', "/").to_ascii_lowercase();
            match options.action {
                ReplaceAction::Exact => material == find,
                ReplaceAction::Partial | ReplaceAction::Substitute => material.contains(&find),
            }
        };
        let sides: BTreeSet<u32> = self
            .solids_where(|_| true)
            .iter()
            .flat_map(|solid| solid.sides())
            .filter(|side| matches(&side.material))
            .map(|side| side.id)
            .filter(|id| options.within.as_ref().is_none_or(|within| within.contains(id)))
            .collect();
        if sides.is_empty() {
            return Err(format!("no face uses {find}"));
        }
        if options.mark_only {
            let count = sides.len();
            self.select_faces(sides, SelectMode::Replace);
            self.message = format!("Marked {count} faces using {find}");
            return Ok(count);
        }
        let replacement = replace.trim().replace('\\', "/");
        if replacement.is_empty() {
            return Err("give the replacement".to_owned());
        }
        // The new material each face gets, and the texture sizes for rescaling.
        let new_material = |material: &str| match options.action {
            ReplaceAction::Substitute => replace_ignoring_case(&material.replace('\\', "/"), &find, &replacement),
            _ => replacement.clone(),
        };
        let mut sizes: HashMap<String, [f64; 2]> = HashMap::new();
        if options.rescale {
            let faces = self.faces(&sides);
            for material in faces
                .iter()
                .flat_map(|face| [face.side.material.clone(), new_material(&face.side.material)])
            {
                if let Entry::Vacant(entry) = sizes.entry(material.to_ascii_lowercase()) {
                    let size = self.texture_size(entry.key());
                    entry.insert(size);
                }
            }
        }
        let count = self.edit_sides(&sides, None, |side, _| {
            let material = new_material(&side.material);
            if options.rescale {
                // Keep the texture the same size in the world: a texture twice as large
                // gets half the scale.
                let old = sizes.get(&side.material.to_ascii_lowercase()).copied();
                let new = sizes.get(&material.to_ascii_lowercase()).copied();
                if let (Some([old_width, old_height]), Some([new_width, new_height])) = (old, new) {
                    side.uaxis.scale *= old_width / new_width.max(1.0);
                    side.vaxis.scale *= old_height / new_height.max(1.0);
                }
            }
            side.material = material;
        })?;
        self.remember_material(&replacement);
        self.message = format!("Replaced {find} on {count} faces");
        Ok(count)
    }

    /// Halves or doubles the texture scale along one axis, keeping the texture pinned at
    /// each face's top-left corner, so a texture lined up with an edge stays lined up.
    pub fn scale_axis(&mut self, sides: &BTreeSet<u32>, horizontal: bool, factor: f64) -> Result<usize, String> {
        self.edit_sides(sides, None, |side, polygon| {
            let axis = if horizontal { &mut side.uaxis } else { &mut side.vaxis };
            let new_scale = axis.scale * factor;
            if new_scale.abs() < 1e-4 {
                return;
            }
            let start = polygon
                .iter()
                .map(|point| point.dot(axis.axis))
                .fold(f64::INFINITY, f64::min);
            if start.is_finite() {
                // The texel at the face's start stays the same.
                let texel = start / axis.scale + axis.offset;
                axis.offset = texel - start / new_scale;
            }
            axis.scale = new_scale;
        })
    }

    /// Every material the map's brushes use, sorted, with how many faces use each.
    pub fn used_materials(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for solid in self.solids_where(|_| true) {
            for side in solid.sides() {
                *counts.entry(side.material.to_ascii_uppercase()).or_default() += 1;
            }
        }
        let mut used: Vec<(String, usize)> = counts.into_iter().collect();
        used.sort();
        used
    }
}

/// A seed that differs every time, for random shifts and hotspot picks.
fn random_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos() as u64)
}

/// A random number for one face, from a seed and the face's side id.
fn face_random(seed: u64, side: u32) -> u64 {
    let mut hasher = DefaultHasher::new();
    (seed, side).hash(&mut hasher);
    hasher.finish()
}

/// Replaces every occurrence of `find` (lower case) in `text`, ignoring case.
fn replace_ignoring_case(text: &str, find: &str, replacement: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut result = String::with_capacity(text.len());
    let mut last = 0;
    for (at, _) in lower.match_indices(find) {
        result.push_str(&text[last..at]);
        result.push_str(replacement);
        last = at + find.len();
    }
    result.push_str(&text[last..]);
    result
}

/// Puts the values Face Edit set on a side; values left `None` stay as they are.
fn set_values(side: &mut Side, values: &FaceValues) {
    if let Some(rotation) = values.rotation {
        side.set_rotation(rotation);
    }
    let axes = [&mut side.uaxis, &mut side.vaxis];
    for (index, axis) in axes.into_iter().enumerate() {
        if let Some(scale) = values.scale[index].filter(|scale| scale.abs() > 1e-6) {
            axis.scale = scale;
        }
        if let Some(shift) = values.shift[index] {
            axis.offset = shift;
        }
        if let Some(direction) = values.axes[index] {
            axis.axis = direction;
        }
    }
    if let Some(lightmap_scale) = values.lightmap_scale {
        side.lightmap_scale = Some(lightmap_scale.max(1));
    }
    if let Some(groups) = values.smoothing_groups {
        side.smoothing_groups = Some(groups);
    }
}

/// A brush's faces with their polygons; sides with no area get an empty polygon.
fn faces_of(solid: &Solid) -> Vec<FaceInfo> {
    let Some(id) = solid.id() else {
        return Vec::new();
    };
    let polygons = solid_faces(solid);
    solid
        .sides()
        .enumerate()
        .map(|(index, side)| FaceInfo {
            solid: id,
            side: side.clone(),
            polygon: polygons
                .iter()
                .find(|face| face.side == index)
                .map(|face| face.vertices.clone())
                .unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_ignoring_case() {
        assert_eq!(
            replace_ignoring_case("BRICK/BrickWall001a", "brick", "concrete"),
            "concrete/concreteWall001a"
        );
        assert_eq!(replace_ignoring_case("metal/floor", "brick", "x"), "metal/floor");
    }
}
