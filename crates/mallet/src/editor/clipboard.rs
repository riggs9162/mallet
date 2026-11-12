//! Hammer's clipboard: Cut, Copy, Paste and Paste Special.
//!
//! Copied objects live on the [`Editor`], outside any one map, so they paste into any
//! tab with fresh ids. Copying also puts them on the system clipboard as VMF text, so
//! another Mallet window can paste them too.

use std::collections::{BTreeSet, HashMap, HashSet};

use glam::{DMat4, DVec2, DVec3};
use mallet_vmf::edit::Edit;
use mallet_vmf::geometry::{faces_bounds, solid_faces, Bounds};
use mallet_vmf::kv::{self, Block, Item};
use mallet_vmf::sides::{CopiedSides, OtherFaces};
use mallet_vmf::transform::{self, around, clean_vec3, TextureLock};
use mallet_vmf::{format_vec3, parse_vec3, Entity, EntityItem, IdAllocator, Solid, SolidItem, Vmf};

use super::{find_entity, Editor, SelectMode};
use crate::scene::{Owner, Remap};
use crate::transform::rotation_xyz;
use crate::view2d::Axis2D;

/// The block that marks VMF text on the system clipboard as copied from Mallet.
const MARKER: &str = "mallet_clipboard";
/// The most copies one Paste Special makes.
pub const MAX_COPIES: u32 = 1000;
/// Keys whose values are never entity names, so renaming pasted entities leaves them alone.
const NOT_NAMES: [&str; 4] = ["id", "classname", "origin", "angles"];

/// Brushes and entities copied with Cut or Copy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Clipboard {
    /// World brushes, as they were in their map.
    pub solids: Vec<Solid>,
    /// Entities, with their own brushes.
    pub entities: Vec<Entity>,
    /// The pivot placed when the objects were copied, which Paste Special can rotate
    /// the copies around.
    pub pivot: Option<DVec3>,
}

impl Clipboard {
    /// Whether nothing has been copied.
    pub fn is_empty(&self) -> bool {
        self.solids.is_empty() && self.entities.is_empty()
    }

    /// How many objects a paste adds.
    pub fn len(&self) -> usize {
        self.solids.len() + self.entities.len()
    }

    /// The box around everything copied: brush faces, and the origins of point entities.
    pub fn bounds(&self) -> Bounds {
        let mut bounds = Bounds::EMPTY;
        for solid in self.solids.iter().chain(self.entities.iter().flat_map(Entity::solids)) {
            bounds.add_bounds(&faces_bounds(&solid_faces(solid)));
        }
        for entity in &self.entities {
            if entity.solids().next().is_none() {
                if let Some(origin) = entity.origin() {
                    bounds.add_point(origin);
                }
            }
        }
        bounds
    }

    /// The objects as VMF text for the system clipboard: a marker block, a `world`
    /// holding the copied world brushes, then one `entity` block per entity.
    pub fn to_text(&self) -> String {
        let mut marker = Block::new(MARKER);
        marker.push_pair("version", "1");
        if let Some(pivot) = self.pivot {
            marker.push_pair("pivot", format_vec3(pivot));
        }
        let world = Entity {
            items: self.solids.iter().cloned().map(EntityItem::Solid).collect(),
        };
        let mut items = vec![Item::Block(marker), Item::Block(world.to_block("world"))];
        items.extend(
            self.entities
                .iter()
                .map(|entity| Item::Block(entity.to_block("entity"))),
        );
        kv::write(&items)
    }

    /// Reads text written by [`Clipboard::to_text`]. Any other text gives `None`.
    pub fn from_text(text: &str) -> Option<Self> {
        let vmf = Vmf::parse(text).ok()?;
        let marker = vmf.block(MARKER)?;
        Some(Self {
            solids: vmf
                .world()
                .map(|world| world.solids().cloned().collect())
                .unwrap_or_default(),
            entities: vmf.entities().cloned().collect(),
            pivot: marker.get("pivot").and_then(parse_vec3),
        })
    }
}

/// Where pasted objects go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PasteAt {
    /// Exactly where they were copied from.
    Original,
    /// Centred on a point in a 2D view, on that view's two axes, keeping their depth.
    /// The move is snapped to the grid, so objects that sat on the grid still do.
    View { axis: Axis2D, point: DVec2 },
    /// Centred on a point in the world.
    Center(DVec3),
    /// Resting on a brush face at `point`, where `normal` points out of the face:
    /// centred on the point across the face and moved out along the normal until they
    /// touch it. The move is snapped to the grid across the face, and along the normal
    /// too unless the face lies square to a world axis.
    Surface { point: DVec3, normal: DVec3 },
}

/// Where the mouse is over a viewport, for Paste to put objects there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PasteCursor {
    /// Over a 2D view, at a grid-snapped point of it.
    View { axis: Axis2D, point: DVec2 },
    /// Over a 3D view, as the ray from the camera through the cursor.
    Ray { origin: DVec3, direction: DVec3 },
}

/// A paste made over a 3D view that still follows the mouse, until a click drops it or
/// Escape takes it back.
#[derive(Clone, Debug)]
pub struct PastePlacing {
    /// Where the objects were pasted, which is where they still are in the map.
    pasted_at: PasteAt,
    /// Where the mouse has taken them since, which is where they are shown.
    target: PasteAt,
    /// The clipboard's bounds when it was pasted.
    bounds: Bounds,
    /// The pasted objects and the scene they were pasted into. Once either changes,
    /// something else has edited the map and the paste stays where it is.
    selection: BTreeSet<Owner>,
    generation: u64,
}

/// Hammer's Paste Special settings, plus Hammer++'s rotation around the pivot.
#[derive(Clone, Debug, PartialEq)]
pub struct PasteOptions {
    /// How many copies to paste.
    pub copies: u32,
    /// Start from where the objects were copied, instead of the paste point.
    pub start_at_original: bool,
    /// How far each copy moves from the one before it.
    pub offset: DVec3,
    /// How far each copy turns from the one before it, in degrees about X, Y and Z.
    pub rotation: DVec3,
    /// Turn the copies around the pivot placed when they were copied, instead of their
    /// centre, as Hammer++ allows.
    pub around_pivot: bool,
    /// Text put before and after the names of pasted entities.
    pub prefix: String,
    pub suffix: String,
    /// Give pasted entities names no other entity in the map has.
    pub unique_names: bool,
    /// Put each copy in a group of its own.
    pub group: bool,
}

impl Default for PasteOptions {
    fn default() -> Self {
        Self {
            copies: 1,
            start_at_original: false,
            offset: DVec3::ZERO,
            rotation: DVec3::ZERO,
            around_pivot: false,
            prefix: String::new(),
            suffix: String::new(),
            unique_names: false,
            group: false,
        }
    }
}

impl Editor {
    /// Copies the selected brushes and entities to the clipboard and the system
    /// clipboard. Returns how many were copied.
    pub fn copy_selection(&mut self) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        if self.selection.is_empty() {
            return Err("nothing is selected".to_owned());
        }
        let world = vmf.world().ok_or("the map has no world")?;
        let mut clipboard = Clipboard {
            pivot: self.overlay.pivot,
            ..Clipboard::default()
        };
        for owner in &self.selection {
            match *owner {
                Owner::Solid(id) => clipboard.solids.push(
                    world
                        .solids()
                        .find(|solid| solid.id() == Some(id))
                        .ok_or_else(|| format!("no world brush has id {id}"))?
                        .clone(),
                ),
                Owner::Entity(id) => clipboard.entities.push(find_entity(vmf, id)?.clone()),
            }
        }
        let count = clipboard.len();
        self.system_clipboard = Some(clipboard.to_text());
        self.clipboard = clipboard;
        self.message = format!("Copied {}", objects(count));
        Ok(count)
    }

    /// Copies the selection, then deletes it, as one undo step for the delete.
    pub fn cut_selection(&mut self) -> Result<usize, String> {
        let count = self.copy_selection()?;
        self.delete_selection()?;
        self.message = format!("Cut {}", objects(count));
        Ok(count)
    }

    /// Takes objects copied in another Mallet window from system clipboard text. Text
    /// that Mallet did not write leaves the clipboard as it is.
    pub fn read_system_clipboard(&mut self, text: &str) {
        if let Some(clipboard) = Clipboard::from_text(text) {
            self.clipboard = clipboard;
        }
    }

    /// Where Paste puts objects: under the cursor in the 2D view it is over, on the brush
    /// face under the cursor in a 3D view, or in the middle of the 2D view used last, as
    /// Hammer does. A 3D view cursor over empty space counts as no cursor.
    pub fn paste_point(&self) -> PasteAt {
        if let Some(at) = self.cursor_paste_point(false) {
            return at;
        }
        let view = &self.views[self.active_view.min(self.views.len() - 1)];
        PasteAt::View {
            axis: view.axis,
            point: view.center,
        }
    }

    /// Where the cursor points for a paste: its point in a 2D view, or the brush face
    /// under it in a 3D view. `behind_selection` looks through the selected brushes.
    fn cursor_paste_point(&self, behind_selection: bool) -> Option<PasteAt> {
        match self.paste_cursor? {
            PasteCursor::View { axis, point } => Some(PasteAt::View { axis, point }),
            PasteCursor::Ray { origin, direction } => {
                let (hit, normal) = if behind_selection {
                    self.scene.surface_face_behind_selection(origin, direction)?
                } else {
                    self.scene.surface_face(origin, direction)?
                };
                Some(PasteAt::Surface {
                    point: hit.point,
                    normal,
                })
            }
        }
    }

    /// Pastes one copy of the clipboard under the cursor. Over a 3D view the pasted
    /// objects then follow the mouse across the faces it points at, until
    /// [`Editor::drop_paste`] leaves them there or [`Editor::cancel_paste`] takes them
    /// back. Over a 2D view, or no view, they are placed at once.
    pub fn paste_at_cursor(&mut self) -> Result<usize, String> {
        let at = self.paste_point();
        let count = self.paste(at, &PasteOptions::default())?;
        if matches!(self.paste_cursor, Some(PasteCursor::Ray { .. })) {
            self.paste_placing = Some(PastePlacing {
                pasted_at: at,
                target: at,
                bounds: self.clipboard.bounds(),
                selection: self.selection.clone(),
                generation: self.scene.geometry_generation,
            });
            self.message = format!(
                "Pasted {}: move the mouse to place, click to drop, Escape to cancel",
                objects(count)
            );
        }
        Ok(count)
    }

    /// Whether a paste is following the mouse. It stops following, and stays where it
    /// is, once anything else has changed the map or the selection.
    fn paste_is_following(&mut self) -> bool {
        let following = self.paste_placing.as_ref().is_some_and(|placing| {
            placing.selection == self.selection && placing.generation == self.scene.geometry_generation
        });
        if !following && self.paste_placing.take().is_some() {
            self.overlay.transform = None;
        }
        following
    }

    /// Shows a paste that is following the mouse where the cursor points now. With the
    /// cursor over no view or no brush face it stays where it was shown last. Returns
    /// whether a paste is following the mouse.
    pub fn follow_paste_cursor(&mut self) -> bool {
        if !self.paste_is_following() {
            return false;
        }
        let target = self.cursor_paste_point(true);
        let grid = self.snap_step();
        if let (Some(target), Some(placing)) = (target, self.paste_placing.as_mut()) {
            placing.target = target;
            let offset = placing.offset(grid);
            self.overlay.transform = (offset != DVec3::ZERO).then(|| Remap::translate(offset));
            self.overlay.cloning = false;
        }
        true
    }

    /// Leaves a paste that is following the mouse where it is shown. The paste stays one
    /// undo step.
    pub fn drop_paste(&mut self) {
        if !self.paste_is_following() {
            return;
        }
        let Some(placing) = self.paste_placing.take() else {
            return;
        };
        self.overlay.transform = None;
        if placing.offset(self.snap_step()) == DVec3::ZERO {
            self.message = format!("Pasted {}", objects(placing.selection.len()));
            return;
        }
        let moved = self
            .step_history(false)
            .and_then(|_| self.paste(placing.target, &PasteOptions::default()));
        if let Err(error) = moved {
            self.message = error;
        }
    }

    /// Takes back a paste that is following the mouse. Returns whether there was one.
    pub fn cancel_paste(&mut self) -> bool {
        if !self.paste_is_following() {
            return false;
        }
        self.paste_placing = None;
        self.overlay.transform = None;
        self.clear_selection();
        if self.step_history(false).is_ok() {
            self.message = "Paste cancelled".to_owned();
        }
        true
    }

    /// Opens Paste Special for the objects on the clipboard, pasting at the current paste
    /// point unless the dialog says to start at the original.
    pub fn open_paste_special(&mut self) -> Result<(), String> {
        if self.clipboard.is_empty() {
            return Err("the clipboard is empty; copy something first".to_owned());
        }
        self.document.as_ref().ok_or("no map is open")?;
        self.paste_special_at = self.paste_point();
        self.paste_special_open = true;
        Ok(())
    }

    /// Pastes the clipboard into the open map as one undo step and selects what was
    /// pasted. Returns how many objects were added.
    ///
    /// Overlays and other entities that list brush faces are pointed at the faces pasted
    /// with them in the same copy. Faces that were not pasted are taken off their lists,
    /// since the map pasted into may use those ids for unrelated faces.
    pub fn paste(&mut self, at: PasteAt, options: &PasteOptions) -> Result<usize, String> {
        if self.clipboard.is_empty() {
            return Err("the clipboard is empty; copy something first".to_owned());
        }
        if options.copies == 0 || options.copies > MAX_COPIES {
            return Err(format!("paste between 1 and {MAX_COPIES} copies"));
        }
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world_id = vmf.world().and_then(Entity::id).ok_or("the map has no world")?;
        let clipboard = &self.clipboard;
        let bounds = clipboard.bounds();
        let at = if options.start_at_original {
            PasteAt::Original
        } else {
            at
        };
        let shift = paste_shift(at, &bounds, self.snap_step());
        let pivot = match clipboard.pivot {
            Some(pivot) if options.around_pivot => pivot,
            _ => bounds.center(),
        };
        let lock = if self.config.transform.texture_lock {
            TextureLock::Full
        } else {
            TextureLock::Off
        };
        let mut ids = IdAllocator::for_map(vmf);
        let mut names = MapNames::new(vmf);
        let mut edits = Vec::new();
        let mut pasted = Vec::new();
        for copy in 1..=options.copies {
            let step = f64::from(copy);
            let turn = options.rotation * step;
            let matrix = DMat4::from_translation(shift + options.offset * step) * around(pivot, rotation_xyz(turn));
            let turned = turn != DVec3::ZERO;
            let group = options.group.then(|| ids.object());
            if let Some(group) = group {
                edits.push(Edit::AddBlock {
                    entity: world_id,
                    index: None,
                    block: group_block(group),
                });
            }
            let renames = names.renames(&clipboard.entities, options);
            let mut sides = CopiedSides::default();
            for source in &clipboard.solids {
                let mut solid = source.clone();
                sides.renumber_solid(&mut solid, &mut ids);
                solid.apply_matrix(&matrix, lock);
                set_group(solid_editor(&mut solid), group);
                pasted.extend(solid.id().map(Owner::Solid));
                edits.push(Edit::AddSolid {
                    entity: world_id,
                    index: None,
                    solid,
                });
            }
            let mut entities = clipboard.entities.clone();
            for entity in &mut entities {
                rename_entity(entity, &renames);
                sides.renumber_entity(entity, &mut ids);
            }
            for mut entity in entities {
                sides.remap_entity(&mut entity, OtherFaces::Drop);
                transform_entity(&mut entity, &matrix, lock, turned);
                set_group(entity_editor(&mut entity), group);
                pasted.extend(entity.id().map(Owner::Entity));
                edits.push(Edit::AddEntity { index: None, entity });
            }
        }
        let count = pasted.len();
        self.apply_edit(&Edit::Batch(edits))?;
        self.select(pasted, SelectMode::Replace);
        self.message = if options.copies == 1 {
            format!("Pasted {}", objects(count))
        } else {
            format!("Pasted {} copies, {}", options.copies, objects(count))
        };
        Ok(count)
    }
}

impl PastePlacing {
    /// How far the mouse has taken the objects from where they were pasted.
    fn offset(&self, grid: f64) -> DVec3 {
        paste_shift(self.target, &self.bounds, grid) - paste_shift(self.pasted_at, &self.bounds, grid)
    }
}

/// `1 object` or `3 objects`.
fn objects(count: usize) -> String {
    if count == 1 {
        "1 object".to_owned()
    } else {
        format!("{count} objects")
    }
}

/// The least a face normal's largest part can be for the face to count as square to a
/// world axis, so objects pasted on it sit exactly on it even when it is off the grid.
const SQUARE_FACE: f64 = 0.999;

/// How far pasting at `at` moves objects whose bounds are `bounds`.
fn paste_shift(at: PasteAt, bounds: &Bounds, grid: f64) -> DVec3 {
    let center = bounds.center();
    match at {
        PasteAt::Original => DVec3::ZERO,
        PasteAt::Center(point) => point - center,
        PasteAt::View { axis, point } => {
            let [horizontal, vertical, _] = axis.axes();
            let mut shift = DVec3::ZERO;
            for (index, wanted) in [(horizontal, point.x), (vertical, point.y)] {
                let corner = bounds.min[index] + wanted - center[index];
                shift[index] = (corner / grid).round() * grid - bounds.min[index];
            }
            shift
        }
        PasteAt::Surface { point, normal } => {
            let facing = normal.abs();
            let out = (0..3).fold(0, |best, index| if facing[index] > facing[best] { index } else { best });
            let mut shift = DVec3::ZERO;
            for index in 0..3 {
                if index != out {
                    let corner = bounds.min[index] + point[index] - center[index];
                    shift[index] = (corner / grid).round() * grid - bounds.min[index];
                    continue;
                }
                let upward = normal[out] > 0.0;
                let resting = if normal[out].abs() > SQUARE_FACE {
                    (point[out] * 1024.0).round() / 1024.0
                } else if upward {
                    (point[out] / grid).ceil() * grid
                } else {
                    (point[out] / grid).floor() * grid
                };
                let touching = if upward { bounds.min[out] } else { bounds.max[out] };
                shift[out] = resting - touching;
            }
            shift
        }
    }
}

/// Moves and turns a pasted entity: its brushes, its origin, an overlay's plane and
/// corners, and its angles when the copy is rotated, as the Transform dialog does.
fn transform_entity(entity: &mut Entity, matrix: &DMat4, lock: TextureLock, turned: bool) {
    let point = entity.solids().next().is_none();
    for solid in entity.solids_mut() {
        solid.apply_matrix(matrix, lock);
    }
    if let Some(origin) = entity.origin() {
        entity.set("origin", format_vec3(clean_vec3(matrix.transform_point3(origin))));
    }
    mallet_vmf::overlay::transform_entity(entity, matrix);
    if !turned {
        return;
    }
    let angles = entity.get("angles").and_then(parse_vec3);
    let Some(start) = angles.or(point.then_some(DVec3::ZERO)) else {
        return;
    };
    if let Some(result) = transform::transform_angles(start, transform::linear_part(matrix)) {
        if Some(result) != angles {
            entity.set("angles", format_vec3(result));
        }
    }
}

/// The entity names already in a map, so pasted names can be told apart from them.
struct MapNames {
    taken: HashSet<String>,
}

impl MapNames {
    fn new(vmf: &Vmf) -> Self {
        Self {
            taken: vmf
                .entities()
                .filter_map(|entity| entity.get("targetname"))
                .map(str::to_ascii_lowercase)
                .collect(),
        }
    }

    /// The new name of each named entity in one copy, by its old name in lower case.
    /// Names that stay the same are left out.
    fn renames(&mut self, entities: &[Entity], options: &PasteOptions) -> HashMap<String, String> {
        let mut renames = HashMap::new();
        if options.prefix.is_empty() && options.suffix.is_empty() && !options.unique_names {
            return renames;
        }
        for name in entities.iter().filter_map(|entity| entity.get("targetname")) {
            let key = name.to_ascii_lowercase();
            if name.is_empty() || renames.contains_key(&key) {
                continue;
            }
            let mut renamed = format!("{}{name}{}", options.prefix, options.suffix);
            if options.unique_names && self.taken.contains(&renamed.to_ascii_lowercase()) {
                let stem = renamed.trim_end_matches(|c: char| c.is_ascii_digit()).to_owned();
                renamed = (1..)
                    .map(|number| format!("{stem}{number}"))
                    .find(|candidate| !self.taken.contains(&candidate.to_ascii_lowercase()))
                    .expect("some numbered name is free");
            }
            self.taken.insert(renamed.to_ascii_lowercase());
            if renamed != name {
                renames.insert(key, renamed);
            }
        }
        renames
    }
}

/// Renames a pasted entity, and points its keys and outputs that name another pasted
/// entity at that entity's new name.
fn rename_entity(entity: &mut Entity, renames: &HashMap<String, String>) {
    if renames.is_empty() {
        return;
    }
    let renamed = |value: &str| renames.get(&value.to_ascii_lowercase()).cloned();
    for item in &mut entity.items {
        if let EntityItem::Key(pair) = item {
            let skip = NOT_NAMES.iter().any(|key| pair.key.eq_ignore_ascii_case(key));
            if let (false, Some(value)) = (skip, renamed(&pair.value)) {
                pair.value = value;
            }
        }
    }
    let mut connections = entity.connections();
    let mut changed = false;
    for connection in &mut connections {
        if let Some(target) = renamed(&connection.target) {
            connection.target = target;
            changed = true;
        }
    }
    if changed {
        entity.set_connections(&connections);
    }
}

/// A world `group` block, as Hammer writes one.
pub(super) fn group_block(id: u32) -> Block {
    let mut editor = Block::new("editor");
    editor.push_pair("color", "0 100 100");
    editor.push_pair("visgroupshown", "1");
    editor.push_pair("visgroupautoshown", "1");
    let mut group = Block::new("group");
    group.push_pair("id", id.to_string());
    group.push_block(editor);
    group
}

/// A brush's `editor` block, added when it has none.
pub(super) fn solid_editor(solid: &mut Solid) -> &mut Block {
    let index = match solid
        .items
        .iter()
        .position(|item| matches!(item, SolidItem::Other(block) if block.name.eq_ignore_ascii_case("editor")))
    {
        Some(index) => index,
        None => {
            solid.items.push(SolidItem::Other(Block::new("editor")));
            solid.items.len() - 1
        }
    };
    match &mut solid.items[index] {
        SolidItem::Other(block) => block,
        _ => unreachable!("the index points at the editor block"),
    }
}

/// An entity's `editor` block, added when it has none.
fn entity_editor(entity: &mut Entity) -> &mut Block {
    let index = match entity
        .items
        .iter()
        .position(|item| matches!(item, EntityItem::Other(block) if block.name.eq_ignore_ascii_case("editor")))
    {
        Some(index) => index,
        None => {
            entity.items.push(EntityItem::Other(Block::new("editor")));
            entity.items.len() - 1
        }
    };
    match &mut entity.items[index] {
        EntityItem::Other(block) => block,
        _ => unreachable!("the index points at the editor block"),
    }
}

/// Puts an object in a group, or takes it out of the group it was copied from, which
/// may not exist in the map it is pasted into.
pub(super) fn set_group(editor: &mut Block, group: Option<u32>) {
    editor
        .items
        .retain(|item| !matches!(item, Item::Pair(pair) if pair.key.eq_ignore_ascii_case("groupid")));
    if let Some(group) = group {
        let after_color = editor
            .items
            .iter()
            .position(|item| matches!(item, Item::Pair(pair) if pair.key.eq_ignore_ascii_case("color")))
            .map_or(0, |index| index + 1);
        editor
            .items
            .insert(after_color, Item::Pair(kv::Pair::new("groupid", group.to_string())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mallet_vmf::build::box_solid;

    fn editor_with_map() -> Editor {
        let mut editor = Editor::default();
        editor.new_map();
        editor
    }

    fn named(editor: &mut Editor, name: &str, target: &str) -> u32 {
        let keys = [("targetname".to_owned(), name.to_owned())];
        let id = editor
            .create_entity("logic_relay", DVec3::new(32.0, 32.0, 32.0), &keys)
            .unwrap();
        if !target.is_empty() {
            let connection = mallet_vmf::Connection {
                output: "OnTrigger".to_owned(),
                target: target.to_owned(),
                input: "Trigger".to_owned(),
                parameter: String::new(),
                delay: 0.0,
                refires: -1,
            };
            editor
                .apply_edit(&Edit::SetConnections {
                    entity: id,
                    connections: vec![connection],
                })
                .unwrap();
        }
        id
    }

    fn vmf(editor: &Editor) -> &Vmf {
        &editor.document.as_ref().unwrap().vmf
    }

    #[test]
    fn copies_survive_the_system_clipboard() {
        let mut ids = IdAllocator::for_map(&Vmf::new_map());
        let clipboard = Clipboard {
            solids: vec![box_solid(DVec3::ZERO, DVec3::splat(64.0), "TOOLS/TOOLSNODRAW", &mut ids).unwrap()],
            entities: Vec::new(),
            pivot: Some(DVec3::new(1.0, 2.0, 3.0)),
        };
        assert_eq!(Clipboard::from_text(&clipboard.to_text()), Some(clipboard));
        assert_eq!(Clipboard::from_text("versioninfo\n{\n}\n"), None);
        assert_eq!(Clipboard::from_text("just some text"), None);
    }

    #[test]
    fn paste_adds_copies_with_new_ids_where_asked() {
        let mut editor = editor_with_map();
        let block = editor.create_block(DVec3::ZERO, DVec3::splat(64.0)).unwrap();
        editor.copy_selection().unwrap();
        let options = PasteOptions::default();
        editor
            .paste(PasteAt::Center(DVec3::new(256.0, 0.0, 32.0)), &options)
            .unwrap();
        assert_eq!(editor.selection.len(), 1);
        assert!(!editor.selection.contains(&Owner::Solid(block)));
        let bounds = editor.scene.selection_bounds();
        assert_eq!(bounds.min, DVec3::new(224.0, -32.0, 0.0));
        editor.step_history(false).unwrap();
        assert_eq!(vmf(&editor).world().unwrap().solids().count(), 1);
    }

    #[test]
    fn paste_at_a_view_point_snaps_to_the_grid() {
        let bounds = Bounds {
            min: DVec3::new(0.0, 0.0, 0.0),
            max: DVec3::new(64.0, 64.0, 64.0),
        };
        let at = PasteAt::View {
            axis: Axis2D::Top,
            point: DVec2::new(100.0, -100.0),
        };
        assert_eq!(paste_shift(at, &bounds, 16.0), DVec3::new(64.0, -128.0, 0.0));
    }

    #[test]
    fn paste_on_a_surface_rests_on_it() {
        let bounds = Bounds {
            min: DVec3::new(0.0, 0.0, 0.0),
            max: DVec3::new(64.0, 64.0, 32.0),
        };
        let floor = PasteAt::Surface {
            point: DVec3::new(100.0, -100.0, 8.0),
            normal: DVec3::Z,
        };
        assert_eq!(paste_shift(floor, &bounds, 16.0), DVec3::new(64.0, -128.0, 8.0));
        let ceiling = PasteAt::Surface {
            point: DVec3::new(100.0, -100.0, 128.0),
            normal: DVec3::NEG_Z,
        };
        assert_eq!(paste_shift(ceiling, &bounds, 16.0), DVec3::new(64.0, -128.0, 96.0));
        let wall = PasteAt::Surface {
            point: DVec3::new(-256.0, 40.0, 80.0),
            normal: DVec3::NEG_X,
        };
        assert_eq!(paste_shift(wall, &bounds, 16.0), DVec3::new(-320.0, 16.0, 64.0));
        let slope = PasteAt::Surface {
            point: DVec3::new(0.0, 0.0, 20.0),
            normal: DVec3::new(0.6, 0.0, 0.8),
        };
        assert_eq!(paste_shift(slope, &bounds, 16.0), DVec3::new(-32.0, -32.0, 32.0));
    }

    #[test]
    fn paste_in_a_3d_view_lands_on_the_face_under_the_cursor() {
        let mut editor = editor_with_map();
        editor.create_block(DVec3::ZERO, DVec3::splat(64.0)).unwrap();
        editor.copy_selection().unwrap();
        editor.paste_cursor = Some(PasteCursor::Ray {
            origin: DVec3::new(32.0, 32.0, 512.0),
            direction: DVec3::NEG_Z,
        });
        let at = editor.paste_point();
        assert!(matches!(at, PasteAt::Surface { normal, .. } if normal == DVec3::Z), "{at:?}");
        editor.paste_cursor = Some(PasteCursor::Ray {
            origin: DVec3::new(32.0, 32.0, 512.0),
            direction: DVec3::Z,
        });
        assert!(matches!(editor.paste_point(), PasteAt::View { .. }));
    }

    fn floor_and_copied_box() -> Editor {
        let mut editor = editor_with_map();
        editor
            .create_block(DVec3::new(-256.0, -256.0, -16.0), DVec3::new(256.0, 256.0, 0.0))
            .unwrap();
        editor
            .create_block(DVec3::new(512.0, 512.0, 0.0), DVec3::new(576.0, 576.0, 32.0))
            .unwrap();
        editor.copy_selection().unwrap();
        editor
    }

    fn point_down_at(editor: &mut Editor, x: f64, y: f64) {
        editor.paste_cursor = Some(PasteCursor::Ray {
            origin: DVec3::new(x, y, 512.0),
            direction: DVec3::NEG_Z,
        });
    }

    #[test]
    fn a_paste_in_a_3d_view_follows_the_mouse_until_dropped() {
        let mut editor = floor_and_copied_box();
        editor.grid = 16.0;
        point_down_at(&mut editor, 0.0, 0.0);
        editor.paste_at_cursor().unwrap();
        assert!(editor.follow_paste_cursor());
        assert_eq!(editor.overlay.transform, None);
        point_down_at(&mut editor, 128.0, -64.0);
        assert!(editor.follow_paste_cursor());
        assert_eq!(
            editor.overlay.transform,
            Some(Remap::translate(DVec3::new(128.0, -64.0, 0.0)))
        );
        point_down_at(&mut editor, 4096.0, 0.0);
        assert!(editor.follow_paste_cursor());
        assert!(editor.overlay.transform.is_some());
        editor.drop_paste();
        assert_eq!(editor.overlay.transform, None);
        assert!(!editor.follow_paste_cursor());
        let bounds = editor.scene.selection_bounds();
        assert_eq!(bounds.min, DVec3::new(96.0, -96.0, 0.0));
        assert_eq!(vmf(&editor).world().unwrap().solids().count(), 3);
        editor.step_history(false).unwrap();
        assert_eq!(vmf(&editor).world().unwrap().solids().count(), 2);
    }

    #[test]
    fn escape_takes_back_a_paste_that_follows_the_mouse() {
        let mut editor = floor_and_copied_box();
        point_down_at(&mut editor, 0.0, 0.0);
        editor.paste_at_cursor().unwrap();
        assert_eq!(vmf(&editor).world().unwrap().solids().count(), 3);
        assert!(editor.cancel_paste());
        assert_eq!(vmf(&editor).world().unwrap().solids().count(), 2);
        assert!(editor.selection.is_empty());
        assert!(!editor.cancel_paste());
    }

    #[test]
    fn a_paste_stops_following_once_the_selection_changes() {
        let mut editor = floor_and_copied_box();
        point_down_at(&mut editor, 0.0, 0.0);
        editor.paste_at_cursor().unwrap();
        editor.clear_selection();
        assert!(!editor.follow_paste_cursor());
        assert!(!editor.cancel_paste());
        assert_eq!(vmf(&editor).world().unwrap().solids().count(), 3);
    }

    #[test]
    fn cut_and_paste_into_another_tab() {
        let mut editor = editor_with_map();
        editor.create_block(DVec3::ZERO, DVec3::splat(64.0)).unwrap();
        editor.cut_selection().unwrap();
        assert_eq!(vmf(&editor).world().unwrap().solids().count(), 0);
        editor.new_tab();
        editor.paste(PasteAt::Original, &PasteOptions::default()).unwrap();
        assert_eq!(vmf(&editor).world().unwrap().solids().count(), 1);
        assert_eq!(editor.scene.selection_bounds().min, DVec3::ZERO);
    }

    #[test]
    fn paste_special_makes_rotated_offset_copies() {
        let mut editor = editor_with_map();
        editor
            .create_block(DVec3::new(64.0, -8.0, 0.0), DVec3::new(128.0, 8.0, 16.0))
            .unwrap();
        editor.set_pivot(Some(DVec3::ZERO));
        editor.copy_selection().unwrap();
        let options = PasteOptions {
            copies: 3,
            start_at_original: true,
            offset: DVec3::new(0.0, 0.0, 16.0),
            rotation: DVec3::new(0.0, 0.0, 90.0),
            around_pivot: true,
            ..PasteOptions::default()
        };
        editor.paste(PasteAt::Original, &options).unwrap();
        assert_eq!(editor.selection.len(), 3);
        let mut tops: Vec<Bounds> = editor
            .selection
            .iter()
            .map(|owner| editor.owner_bounds(*owner))
            .collect();
        tops.sort_by(|a, b| a.min.z.total_cmp(&b.min.z));
        assert_eq!(tops[0].min.round(), DVec3::new(-8.0, 64.0, 16.0));
        assert_eq!(tops[1].min.round(), DVec3::new(-128.0, -8.0, 32.0));
        assert_eq!(tops[2].min.round(), DVec3::new(-8.0, -128.0, 48.0));
    }

    #[test]
    fn paste_special_renames_and_rewires_entities() {
        let mut editor = editor_with_map();
        let door = named(&mut editor, "door", "");
        let relay = named(&mut editor, "relay", "door");
        editor.select([Owner::Entity(door), Owner::Entity(relay)], SelectMode::Replace);
        editor.copy_selection().unwrap();
        let options = PasteOptions {
            copies: 2,
            start_at_original: true,
            unique_names: true,
            ..PasteOptions::default()
        };
        editor.paste(PasteAt::Original, &options).unwrap();
        let mut names: Vec<(String, String)> = vmf(&editor)
            .entities()
            .map(|entity| {
                let target = entity
                    .connections()
                    .first()
                    .map(|c| c.target.clone())
                    .unwrap_or_default();
                (entity.get("targetname").unwrap().to_owned(), target)
            })
            .collect();
        names.sort();
        let expected = [
            ("door", ""),
            ("door1", ""),
            ("door2", ""),
            ("relay", "door"),
            ("relay1", "door1"),
            ("relay2", "door2"),
        ];
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn paste_special_groups_each_copy() {
        let mut editor = editor_with_map();
        editor.create_block(DVec3::ZERO, DVec3::splat(64.0)).unwrap();
        editor.copy_selection().unwrap();
        let options = PasteOptions {
            copies: 2,
            offset: DVec3::new(128.0, 0.0, 0.0),
            start_at_original: true,
            group: true,
            ..PasteOptions::default()
        };
        editor.paste(PasteAt::Original, &options).unwrap();
        let world = vmf(&editor).world().unwrap();
        let groups: Vec<String> = world
            .items
            .iter()
            .filter_map(|item| match item {
                EntityItem::Other(block) if block.name == "group" => block.get("id").map(str::to_owned),
                _ => None,
            })
            .collect();
        assert_eq!(groups.len(), 2);
        let members: Vec<String> = world
            .solids()
            .filter_map(|solid| solid.editor()?.get("groupid").map(str::to_owned))
            .collect();
        assert_eq!(members, groups);
        editor.step_history(false).unwrap();
        let world = vmf(&editor).world().unwrap();
        assert_eq!(world.solids().count(), 1);
        assert!(!world
            .items
            .iter()
            .any(|item| matches!(item, EntityItem::Other(b) if b.name == "group")));
    }
}
