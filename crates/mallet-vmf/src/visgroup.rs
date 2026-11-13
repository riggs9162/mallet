//! Hammer's VisGroups: named sets of objects that are shown and hidden together.
//!
//! A map lists its groups in the top-level `visgroups` block. A brush or entity names the
//! groups it is in with `visgroupid` keys in its `editor` block, where `visgroupshown` and
//! `visgroupautoshown` say whether a group of the user or an automatic group hides it. An
//! object either of them hides is also wrapped in a `hidden` block, the way
//! [`Edit::SetHidden`] writes it, so the editor's views skip it.
//!
//! Automatic groups are not stored: [`AutoGroup`] sorts every object by what it is, as
//! Hammer's Auto tab does.

use std::collections::HashMap;

use crate::edit::{child_id, is_hidden_block, Edit};
use crate::kv::{Block, Item, Pair};
use crate::{parse_color, Entity, EntityItem, Solid, TopLevel, Vmf};

/// The colour Hammer gives a group that has none.
pub const DEFAULT_COLOR: [u8; 3] = [128, 128, 128];

/// One VisGroup of the map, with the groups nested inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisGroup {
    /// The `visgroupid` its members name.
    pub id: u32,
    /// The name shown in the VisGroups list.
    pub name: String,
    /// The colour of its members in the 2D views.
    pub color: [u8; 3],
    /// Groups nested inside this one.
    pub children: Vec<VisGroup>,
}

impl VisGroup {
    fn from_block(block: &Block) -> Option<Self> {
        Some(Self {
            id: block.get("visgroupid")?.trim().parse().ok()?,
            name: block.get("name").unwrap_or_default().to_owned(),
            color: block.get("color").and_then(parse_color).unwrap_or(DEFAULT_COLOR),
            children: block.blocks_named("visgroup").filter_map(Self::from_block).collect(),
        })
    }

    fn to_block(&self) -> Block {
        let mut block = Block::new("visgroup");
        block.push_pair("name", self.name.clone());
        block.push_pair("visgroupid", self.id.to_string());
        let [red, green, blue] = self.color;
        block.push_pair("color", format!("{red} {green} {blue}"));
        for child in &self.children {
            block.push_block(child.to_block());
        }
        block
    }

    /// The ids of this group and of every group nested inside it.
    pub fn ids(&self) -> Vec<u32> {
        let mut ids = vec![self.id];
        for child in &self.children {
            ids.extend(child.ids());
        }
        ids
    }
}

/// The map's VisGroups, in the order of its `visgroups` block.
pub fn groups(vmf: &Vmf) -> Vec<VisGroup> {
    vmf.block("visgroups")
        .map(|block| {
            block
                .blocks_named("visgroup")
                .filter_map(VisGroup::from_block)
                .collect()
        })
        .unwrap_or_default()
}

/// The group with this id, wherever it is nested.
pub fn find(groups: &[VisGroup], id: u32) -> Option<&VisGroup> {
    groups.iter().find_map(|group| {
        if group.id == id {
            Some(group)
        } else {
            find(&group.children, id)
        }
    })
}

/// The edit that makes `groups` the map's VisGroups.
pub fn set_groups(groups: &[VisGroup]) -> Edit {
    let mut block = Block::new("visgroups");
    for group in groups {
        block.push_block(group.to_block());
    }
    Edit::SetVisGroups(Some(block))
}

/// An id no group of the map uses yet.
pub fn free_id(groups: &[VisGroup]) -> u32 {
    groups.iter().flat_map(VisGroup::ids).max().unwrap_or(0) + 1
}

/// A group Hammer's Auto tab fills by itself, from what each object is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoGroup {
    /// Every brush of the world.
    WorldGeometry,
    /// World brushes with a displacement on a face.
    Displacements,
    /// World brushes with a skybox face.
    Skybox,
    /// Brushes textured with tool materials only.
    ToolBrushes,
    /// Tool brushes with a clip material.
    Clips,
    /// Tool brushes with a hint or skip material.
    Hints,
    /// Tool brushes with an areaportal material.
    Areaportals,
    /// Tool brushes with the invisible material.
    Invisible,
    /// Tool brushes that are all nodraw.
    Nodraw,
    /// Every entity.
    Entities,
    /// Entities without brushes.
    PointEntities,
    /// Entities made of brushes.
    BrushEntities,
    /// Light entities.
    Lights,
    /// AI nodes and hints.
    Nodes,
    /// Trigger entities.
    Triggers,
    /// Detail brushes and props.
    WorldDetails,
    /// `func_detail` brushes.
    FuncDetail,
    /// Prop entities.
    Props,
}

impl AutoGroup {
    /// Every automatic group, each parent before its children.
    pub const ALL: [Self; 18] = [
        Self::WorldGeometry,
        Self::Displacements,
        Self::Skybox,
        Self::ToolBrushes,
        Self::Clips,
        Self::Hints,
        Self::Areaportals,
        Self::Invisible,
        Self::Nodraw,
        Self::Entities,
        Self::PointEntities,
        Self::BrushEntities,
        Self::Lights,
        Self::Nodes,
        Self::Triggers,
        Self::WorldDetails,
        Self::FuncDetail,
        Self::Props,
    ];

    /// The name shown in the Auto list.
    pub fn label(self) -> &'static str {
        match self {
            Self::WorldGeometry => "World Geometry",
            Self::Displacements => "Displacements",
            Self::Skybox => "Skybox",
            Self::ToolBrushes => "Tool Brushes",
            Self::Clips => "Clips",
            Self::Hints => "Hints and Skips",
            Self::Areaportals => "Areaportals",
            Self::Invisible => "Invisible",
            Self::Nodraw => "Nodraw",
            Self::Entities => "Entities",
            Self::PointEntities => "Point Entities",
            Self::BrushEntities => "Brush Entities",
            Self::Lights => "Lights",
            Self::Nodes => "Nodes",
            Self::Triggers => "Triggers",
            Self::WorldDetails => "World Details",
            Self::FuncDetail => "Func Detail",
            Self::Props => "Props",
        }
    }

    /// The name agents and settings use for the group.
    pub fn key(self) -> &'static str {
        match self {
            Self::WorldGeometry => "world_geometry",
            Self::Displacements => "displacements",
            Self::Skybox => "skybox",
            Self::ToolBrushes => "tool_brushes",
            Self::Clips => "clips",
            Self::Hints => "hints",
            Self::Areaportals => "areaportals",
            Self::Invisible => "invisible",
            Self::Nodraw => "nodraw",
            Self::Entities => "entities",
            Self::PointEntities => "point_entities",
            Self::BrushEntities => "brush_entities",
            Self::Lights => "lights",
            Self::Nodes => "nodes",
            Self::Triggers => "triggers",
            Self::WorldDetails => "world_details",
            Self::FuncDetail => "func_detail",
            Self::Props => "props",
        }
    }

    /// The group named `key` by [`AutoGroup::key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|group| group.key().eq_ignore_ascii_case(key))
    }

    /// The group this one is listed under.
    pub fn parent(self) -> Option<Self> {
        match self {
            Self::Displacements | Self::Skybox => Some(Self::WorldGeometry),
            Self::Clips | Self::Hints | Self::Areaportals | Self::Invisible | Self::Nodraw => Some(Self::ToolBrushes),
            Self::PointEntities | Self::BrushEntities | Self::Lights | Self::Nodes | Self::Triggers => {
                Some(Self::Entities)
            }
            Self::FuncDetail | Self::Props => Some(Self::WorldDetails),
            Self::WorldGeometry | Self::ToolBrushes | Self::Entities | Self::WorldDetails => None,
        }
    }

    fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// A brush of the world or an entity, as VisGroups see it. Hidden objects are included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// Whether this is a world brush (`true`) or an entity (`false`).
    pub solid: bool,
    /// The brush or entity id.
    pub id: u32,
    /// Whether the object is wrapped in a `hidden` block.
    pub hidden: bool,
    /// The VisGroups it is in.
    pub groups: Vec<u32>,
    /// Whether none of its VisGroups hides it.
    pub shown: bool,
    /// Whether no automatic group hides it.
    pub auto_shown: bool,
    auto: u32,
}

impl Object {
    /// Whether the object belongs to an automatic group.
    pub fn is_in(&self, group: AutoGroup) -> bool {
        self.auto & group.bit() != 0
    }

    /// Whether QuickHide hid the object, and no group did.
    pub fn quick_hidden(&self) -> bool {
        self.hidden && self.shown && self.auto_shown
    }
}

/// Every world brush and entity of the map, hidden ones included, in file order.
pub fn objects(vmf: &Vmf) -> Vec<Object> {
    let mut objects = Vec::new();
    for item in &vmf.items {
        match item {
            TopLevel::World(world) => {
                for item in &world.items {
                    match item {
                        EntityItem::Solid(solid) => objects.extend(solid_object(solid)),
                        EntityItem::Other(block) if is_hidden_block(block) => {
                            objects.extend(block.blocks().filter_map(hidden_solid_object));
                        }
                        _ => {}
                    }
                }
            }
            TopLevel::Entity(entity) => objects.extend(entity_object(entity)),
            TopLevel::Other(block) if is_hidden_block(block) => {
                objects.extend(block.blocks().filter_map(hidden_entity_object));
            }
            _ => {}
        }
    }
    objects
}

fn object(solid: bool, id: u32, hidden: bool, editor: Option<&Block>, auto: u32) -> Object {
    Object {
        solid,
        id,
        hidden,
        groups: editor.map(group_ids).unwrap_or_default(),
        shown: flag(editor, "visgroupshown"),
        auto_shown: flag(editor, "visgroupautoshown"),
        auto,
    }
}

fn solid_object(solid: &Solid) -> Option<Object> {
    let mut materials = solid.sides().map(|side| side.material.as_str());
    let displaced = solid.sides().any(|side| side.dispinfo().is_some());
    let auto = AutoGroup::WorldGeometry.bit() | brush_groups(&mut materials, displaced, true);
    Some(object(true, solid.id()?, false, solid.editor(), auto))
}

fn hidden_solid_object(block: &Block) -> Option<Object> {
    let id = child_id(block, "solid")?;
    let mut materials = block.blocks_named("side").filter_map(|side| side.get("material"));
    let displaced = block
        .blocks_named("side")
        .any(|side| side.blocks_named("dispinfo").next().is_some());
    let auto = AutoGroup::WorldGeometry.bit() | brush_groups(&mut materials, displaced, true);
    Some(object(true, id, true, block.blocks_named("editor").next(), auto))
}

fn entity_object(entity: &Entity) -> Option<Object> {
    let mut materials = entity
        .solids()
        .flat_map(|solid| solid.sides().map(|side| side.material.as_str()));
    let brushes = entity.solids().next().is_some();
    let auto = entity_groups(entity.classname(), brushes, &mut materials);
    Some(object(false, entity.id()?, false, entity.editor(), auto))
}

fn hidden_entity_object(block: &Block) -> Option<Object> {
    let id = child_id(block, "entity")?;
    let mut materials = block
        .blocks_named("solid")
        .flat_map(|solid| solid.blocks_named("side").filter_map(|side| side.get("material")));
    let brushes = block.blocks_named("solid").next().is_some();
    let auto = entity_groups(block.get("classname").unwrap_or_default(), brushes, &mut materials);
    Some(object(false, id, true, block.blocks_named("editor").next(), auto))
}

/// The automatic groups a brush, or the brushes of an entity, belong to by their
/// materials. `world` adds the groups only world brushes are sorted into.
fn brush_groups(materials: &mut dyn Iterator<Item = &str>, displaced: bool, world: bool) -> u32 {
    let mut groups = 0;
    let mut count = 0;
    let mut tools = 0;
    let mut nodraw = 0;
    let mut kinds = 0;
    for material in materials {
        let material = material.replace('\\', "/").to_ascii_lowercase();
        count += 1;
        let Some(tool) = material.strip_prefix("tools/") else {
            continue;
        };
        tools += 1;
        if tool == "toolsnodraw" {
            nodraw += 1;
        } else if tool.contains("clip") {
            kinds |= AutoGroup::Clips.bit();
        } else if tool == "toolshint" || tool == "toolsskip" {
            kinds |= AutoGroup::Hints.bit();
        } else if tool.contains("areaportal") {
            kinds |= AutoGroup::Areaportals.bit();
        } else if tool == "toolsinvisible" {
            kinds |= AutoGroup::Invisible.bit();
        } else if tool.starts_with("toolsskybox") && world {
            groups |= AutoGroup::Skybox.bit();
        }
    }
    if displaced && world {
        groups |= AutoGroup::Displacements.bit();
    }
    if count > 0 && tools == count {
        groups |= AutoGroup::ToolBrushes.bit() | kinds;
        if nodraw == count {
            groups |= AutoGroup::Nodraw.bit();
        }
    }
    groups
}

/// The automatic groups an entity belongs to.
fn entity_groups(classname: &str, brushes: bool, materials: &mut dyn Iterator<Item = &str>) -> u32 {
    let classname = classname.to_ascii_lowercase();
    let mut groups = AutoGroup::Entities.bit();
    if brushes {
        groups |= AutoGroup::BrushEntities.bit() | brush_groups(materials, false, false);
    } else {
        groups |= AutoGroup::PointEntities.bit();
    }
    if classname.starts_with("trigger_") {
        groups |= AutoGroup::Triggers.bit();
    }
    if classname == "func_detail" {
        groups |= AutoGroup::WorldDetails.bit() | AutoGroup::FuncDetail.bit();
    }
    if classname.starts_with("prop_") {
        groups |= AutoGroup::WorldDetails.bit() | AutoGroup::Props.bit();
    }
    if classname.starts_with("light") || classname == "env_projectedtexture" {
        groups |= AutoGroup::Lights.bit();
    }
    if classname.starts_with("info_node") || classname == "info_hint" {
        groups |= AutoGroup::Nodes.bit();
    }
    groups
}

/// The VisGroups an `editor` block names.
pub fn group_ids(editor: &Block) -> Vec<u32> {
    editor
        .pairs()
        .filter(|pair| pair.key.eq_ignore_ascii_case("visgroupid"))
        .filter_map(|pair| pair.value.trim().parse().ok())
        .collect()
}

/// A flag of an `editor` block that is on unless the block says "0".
fn flag(editor: Option<&Block>, key: &str) -> bool {
    editor
        .and_then(|editor| editor.get(key))
        .is_none_or(|value| value.trim() != "0")
}

/// What to change on one object. Fields left `None` keep their value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Whether the object is a brush (`true`) or an entity (`false`).
    pub solid: bool,
    /// The brush or entity id.
    pub id: u32,
    /// The VisGroups it is in from now on.
    pub groups: Option<Vec<u32>>,
    /// Whether its VisGroups show it.
    pub shown: Option<bool>,
    /// Whether the automatic groups show it.
    pub auto_shown: Option<bool>,
    /// The colour it is drawn with in the 2D views, as when it joins a group.
    pub color: Option<[u8; 3]>,
}

impl Change {
    /// A change to the object `object` that keeps everything until a field is set.
    pub fn of(object: &Object) -> Self {
        Self {
            solid: object.solid,
            id: object.id,
            groups: None,
            shown: None,
            auto_shown: None,
            color: None,
        }
    }
}

/// Builds the edit that makes `changes`: it rewrites the objects' `editor` blocks, hides
/// the objects a group now hides and shows the ones no group hides any more. An object
/// QuickHide hid stays hidden until a group that shows it is switched. Returns `None`
/// when nothing would change.
pub fn change_objects(vmf: &Vmf, changes: &[Change]) -> Option<Edit> {
    let editors = editor_blocks(vmf);
    let known: HashMap<(bool, u32), Object> = objects(vmf)
        .into_iter()
        .map(|object| ((object.solid, object.id), object))
        .collect();
    let mut solid_editors = Vec::new();
    let mut entity_editors = Vec::new();
    let mut show = (Vec::new(), Vec::new());
    let mut hide = (Vec::new(), Vec::new());
    for change in changes {
        let Some(object) = known.get(&(change.solid, change.id)) else {
            continue;
        };
        let groups = change.groups.clone().unwrap_or_else(|| object.groups.clone());
        let shown = change.shown.unwrap_or(object.shown);
        let auto_shown = change.auto_shown.unwrap_or(object.auto_shown);
        let flags_changed = shown != object.shown || auto_shown != object.auto_shown;
        let old_editor = editors.get(&(change.solid, change.id)).copied();
        let recolored = change.color.is_some_and(|color| {
            old_editor.and_then(|editor| editor.get("color")).and_then(parse_color) != Some(color)
        });
        if groups != object.groups || flags_changed || recolored {
            let mut editor = old_editor.cloned().unwrap_or_else(|| Block::new("editor"));
            if let Some([red, green, blue]) = change.color.filter(|_| recolored) {
                write_color(&mut editor, format!("{red} {green} {blue}"));
            }
            write_editor(&mut editor, &groups, shown, auto_shown);
            let list = if change.solid {
                &mut solid_editors
            } else {
                &mut entity_editors
            };
            list.push((change.id, Some(editor)));
        }
        let hidden = if flags_changed {
            !(shown && auto_shown)
        } else {
            object.hidden
        };
        if hidden != object.hidden {
            let (solids, entities) = if hidden { &mut hide } else { &mut show };
            if change.solid {
                solids.push(change.id);
            } else {
                entities.push(change.id);
            }
        }
    }
    let mut edits = Vec::new();
    if !solid_editors.is_empty() || !entity_editors.is_empty() {
        edits.push(Edit::SetEditors {
            solids: solid_editors,
            entities: entity_editors,
        });
    }
    for ((solids, entities), hidden) in [(show, false), (hide, true)] {
        if !solids.is_empty() || !entities.is_empty() {
            edits.push(Edit::SetHidden {
                solids,
                entities,
                hidden,
            });
        }
    }
    (!edits.is_empty()).then_some(Edit::Batch(edits))
}

/// The `editor` block of every world brush and entity that has one, hidden ones included.
fn editor_blocks(vmf: &Vmf) -> HashMap<(bool, u32), &Block> {
    let mut editors = HashMap::new();
    for item in &vmf.items {
        match item {
            TopLevel::World(world) => {
                for item in &world.items {
                    match item {
                        EntityItem::Solid(solid) => {
                            if let (Some(id), Some(editor)) = (solid.id(), solid.editor()) {
                                editors.entry((true, id)).or_insert(editor);
                            }
                        }
                        EntityItem::Other(block) if is_hidden_block(block) => {
                            for child in block.blocks() {
                                if let (Some(id), Some(editor)) = (child_id(child, "solid"), raw_editor(child)) {
                                    editors.entry((true, id)).or_insert(editor);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            TopLevel::Entity(entity) => {
                if let (Some(id), Some(editor)) = (entity.id(), entity.editor()) {
                    editors.entry((false, id)).or_insert(editor);
                }
            }
            TopLevel::Other(block) if is_hidden_block(block) => {
                for child in block.blocks() {
                    if let (Some(id), Some(editor)) = (child_id(child, "entity"), raw_editor(child)) {
                        editors.entry((false, id)).or_insert(editor);
                    }
                }
            }
            _ => {}
        }
    }
    editors
}

/// The `editor` block of a brush or entity kept as a plain block while hidden.
fn raw_editor(object: &Block) -> Option<&Block> {
    object.blocks_named("editor").next()
}

/// Sets the colour of an `editor` block, which Hammer writes first.
fn write_color(editor: &mut Block, color: String) {
    let existing = editor.items.iter_mut().find_map(|item| match item {
        Item::Pair(pair) if pair.key.eq_ignore_ascii_case("color") => Some(pair),
        _ => None,
    });
    match existing {
        Some(pair) => pair.value = color,
        None => editor.items.insert(0, Item::Pair(Pair::new("color", color))),
    }
}

/// Writes an object's VisGroups and the two shown flags into its `editor` block: the
/// groups where the first one was, or after the colour, as Hammer orders them.
fn write_editor(editor: &mut Block, groups: &[u32], shown: bool, auto_shown: bool) {
    let is_key = |item: &Item, key: &str| matches!(item, Item::Pair(pair) if pair.key.eq_ignore_ascii_case(key));
    let first = editor.items.iter().position(|item| is_key(item, "visgroupid"));
    editor.items.retain(|item| !is_key(item, "visgroupid"));
    let at = first.unwrap_or_else(|| {
        editor
            .items
            .iter()
            .position(|item| is_key(item, "color"))
            .map_or(0, |index| index + 1)
    });
    for (offset, group) in groups.iter().enumerate() {
        let pair = Item::Pair(Pair::new("visgroupid", group.to_string()));
        editor.items.insert((at + offset).min(editor.items.len()), pair);
    }
    for (key, on) in [("visgroupshown", shown), ("visgroupautoshown", auto_shown)] {
        let value = if on { "1" } else { "0" };
        let existing = editor.items.iter_mut().find_map(|item| match item {
            Item::Pair(pair) if pair.key.eq_ignore_ascii_case(key) => Some(pair),
            _ => None,
        });
        match existing {
            Some(pair) => pair.value = value.to_owned(),
            None => editor.push_pair(key, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::History;

    const MAP: &str = r#"
visgroups
{
	visgroup
	{
		"name" "walls"
		"visgroupid" "4"
		"color" "10 20 30"
		visgroup
		{
			"name" "inner"
			"visgroupid" "7"
			"color" "1 2 3"
		}
	}
}
world
{
	"id" "1"
	"classname" "worldspawn"
	solid
	{
		"id" "2"
		side
		{
			"id" "1"
			"plane" "(0 0 0) (0 64 0) (64 64 0)"
			"material" "TOOLS/TOOLSNODRAW"
			"uaxis" "[1 0 0 0] 0.25"
			"vaxis" "[0 -1 0 0] 0.25"
		}
		editor
		{
			"color" "0 180 0"
			"visgroupid" "4"
			"visgroupshown" "1"
			"visgroupautoshown" "1"
		}
	}
}
entity
{
	"id" "3"
	"classname" "light_spot"
}
"#;

    fn object_of(vmf: &Vmf, solid: bool, id: u32) -> Object {
        objects(vmf)
            .into_iter()
            .find(|object| object.solid == solid && object.id == id)
            .unwrap()
    }

    #[test]
    fn reads_groups_members_and_automatic_groups() {
        let vmf = Vmf::parse(MAP).unwrap();
        let groups = groups(&vmf);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].ids(), [4, 7]);
        assert_eq!(groups[0].color, [10, 20, 30]);
        assert_eq!(free_id(&groups), 8);
        let brush = object_of(&vmf, true, 2);
        assert_eq!(brush.groups, [4]);
        assert!(brush.is_in(AutoGroup::Nodraw) && brush.is_in(AutoGroup::ToolBrushes));
        let light = object_of(&vmf, false, 3);
        assert!(light.is_in(AutoGroup::Lights) && light.is_in(AutoGroup::PointEntities));
        assert!(!light.is_in(AutoGroup::WorldGeometry));
    }

    #[test]
    fn hiding_through_a_group_round_trips_and_undoes() {
        let mut vmf = Vmf::parse(MAP).unwrap();
        let original = vmf.to_text();
        let mut history = History::default();
        let hide: Vec<Change> = objects(&vmf)
            .iter()
            .map(|object| Change {
                groups: Some(vec![4, 7]),
                shown: Some(false),
                ..Change::of(object)
            })
            .collect();
        let edit = change_objects(&vmf, &hide).unwrap();
        history.apply(&mut vmf, &edit).unwrap();
        let reread = Vmf::parse(&vmf.to_text()).unwrap();
        for (solid, id) in [(true, 2), (false, 3)] {
            let object = object_of(&reread, solid, id);
            assert!(object.hidden && !object.shown && !object.quick_hidden());
            assert_eq!(object.groups, [4, 7]);
        }
        assert!(object_of(&reread, true, 2).is_in(AutoGroup::Nodraw));
        assert!(object_of(&reread, false, 3).is_in(AutoGroup::Lights));

        let show: Vec<Change> = objects(&vmf)
            .iter()
            .map(|object| Change {
                shown: Some(true),
                ..Change::of(object)
            })
            .collect();
        let edit = change_objects(&vmf, &show).unwrap();
        history.apply(&mut vmf, &edit).unwrap();
        assert!(objects(&vmf).iter().all(|object| !object.hidden && object.shown));
        assert!(change_objects(&vmf, &show).is_none());

        assert!(history.undo(&mut vmf).unwrap());
        assert!(history.undo(&mut vmf).unwrap());
        assert_eq!(vmf.to_text(), original);
    }

    #[test]
    fn replacing_the_group_list_undoes() {
        let mut vmf = Vmf::parse(MAP).unwrap();
        let original = vmf.to_text();
        let mut history = History::default();
        let mut list = groups(&vmf);
        list.push(VisGroup {
            id: free_id(&list),
            name: "props".to_owned(),
            color: [200, 100, 0],
            children: Vec::new(),
        });
        history.apply(&mut vmf, &set_groups(&list)).unwrap();
        assert_eq!(groups(&vmf), list);
        assert!(find(&groups(&vmf), 8).is_some());
        assert!(history.undo(&mut vmf).unwrap());
        assert_eq!(vmf.to_text(), original);
    }
}
