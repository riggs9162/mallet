//! Reversible edits and an undo history.
//!
//! Every change to a map, whether it comes from the editor UI or from an automation client
//! such as an MCP server, is expressed as an [`Edit`]. Applying an edit returns its inverse,
//! which is what makes undo and redo work the same way for every caller.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use glam::{DMat4, DVec3};

use crate::geometry::Bounds;
use crate::kv::{Block, Item};
use crate::transform::{self, TextureLock};
use crate::{Connection, Entity, EntityItem, Solid, SolidItem, TopLevel, Vmf};

/// One reversible change to a map.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Sets a key on the entity with the given id, adding it if missing.
    SetKey {
        /// Target entity id; the world has id 1 in maps Hammer saves.
        entity: u32,
        /// Key name.
        key: String,
        /// New value.
        value: String,
    },
    /// Removes a key from the entity with the given id.
    ///
    /// Undoing a removal re-adds the key after the entity's last key, so its position in
    /// the file may change.
    RemoveKey {
        /// Target entity id.
        entity: u32,
        /// Key name.
        key: String,
    },
    /// Replaces all outputs of the entity with the given id.
    SetConnections {
        /// Target entity id.
        entity: u32,
        /// The new outputs, in order.
        connections: Vec<Connection>,
    },
    /// Adds a brush to the world or to a brush entity.
    AddSolid {
        /// Owning entity id; use the world's id for world brushes.
        entity: u32,
        /// Position among the entity's items, or `None` to add it after its last brush.
        index: Option<usize>,
        /// The brush, which must already have unique ids.
        solid: Solid,
    },
    /// Removes the brush with the given id from whichever entity owns it.
    RemoveSolid {
        /// Brush id.
        solid: u32,
    },
    /// Adds an entity.
    AddEntity {
        /// Position among the map's top-level items, or `None` to add it after the last entity.
        index: Option<usize>,
        /// The entity, which must already have unique ids.
        entity: Entity,
    },
    /// Removes the entity with the given id. The world cannot be removed.
    RemoveEntity {
        /// Entity id.
        entity: u32,
    },
    /// Adds a block other than a brush, such as a world `group`, to the world or an entity.
    AddBlock {
        /// Owning entity id; use the world's id for world blocks.
        entity: u32,
        /// Position among the entity's items, or `None` to add it after the last item.
        index: Option<usize>,
        /// The block.
        block: Block,
    },
    /// Removes the block at `index` among an entity's items. The item there must be a
    /// block other than a brush.
    RemoveBlock {
        /// Owning entity id.
        entity: u32,
        /// Position among the entity's items.
        index: usize,
    },
    /// Moves brushes with texture lock. Entity origins are moved with [`Edit::SetKey`].
    Translate {
        /// Brush ids.
        solids: Vec<u32>,
        /// Distance to move.
        offset: DVec3,
    },
    /// Stretches brushes so that the box `from` maps onto the box `to`, as when dragging a
    /// selection handle. Undoing restores the original brushes exactly.
    Rescale {
        /// Brush ids.
        solids: Vec<u32>,
        /// The box before resizing, normally the selection's bounds.
        from: Bounds,
        /// The box after resizing.
        to: Bounds,
    },
    /// Applies an affine transform to brushes: rotating, scaling, skewing or mirroring
    /// them, with textures following as `lock` says. Undoing restores the original
    /// brushes exactly. Entity origins and angles are changed with [`Edit::SetKey`].
    Transform {
        /// Brush ids.
        solids: Vec<u32>,
        /// The transform, which must not flatten the brushes.
        matrix: DMat4,
        /// How textures follow the brushes.
        lock: TextureLock,
    },
    /// Replaces brushes with new versions that have the same ids. This is how resizes are
    /// undone without rounding errors.
    ReplaceSolids(Vec<Solid>),
    /// Hides brushes and entities, or shows them again. A hidden object is wrapped in a
    /// `hidden` block where it stands, as Hammer writes it: the map keeps and saves it,
    /// but it leaves the editor's views and selection until shown again. Ids already in
    /// the requested state are left alone. Undoing puts the same objects back.
    SetHidden {
        /// Brush ids.
        solids: Vec<u32>,
        /// Entity ids. The world cannot be hidden.
        entities: Vec<u32>,
        /// Whether the objects become hidden (`true`) or visible (`false`).
        hidden: bool,
    },
    /// Replaces the `editor` blocks of brushes and entities, which hold their colour, their
    /// group and their VisGroups. `None` removes the block. Hidden objects are changed
    /// where they are kept, without showing them.
    SetEditors {
        /// Brush ids, each with its new `editor` block.
        solids: Vec<(u32, Option<Block>)>,
        /// Entity ids, each with its new `editor` block.
        entities: Vec<(u32, Option<Block>)>,
    },
    /// Replaces the map's `visgroups` block, the list of its VisGroups. `None` removes it.
    SetVisGroups(Option<Block>),
    /// Several edits applied in order and undone in reverse as a single step.
    Batch(Vec<Edit>),
}

/// Why an edit could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// No world or entity has this id.
    NoSuchEntity(u32),
    /// No brush has this id.
    NoSuchSolid(u32),
    /// The entity has no such key to remove.
    NoSuchKey {
        /// Target entity id.
        entity: u32,
        /// Missing key.
        key: String,
    },
    /// The world entity is part of every map and cannot be removed.
    CannotRemoveWorld,
    /// A brush or entity being added has no numeric id, so it could not be undone.
    MissingId,
    /// A resize would flatten or turn a box inside out.
    InvalidResize,
    /// A transform would flatten the brushes or is not a finite number.
    InvalidTransform,
    /// The entity has no block other than a brush at this position.
    NoSuchBlock {
        /// Owning entity id.
        entity: u32,
        /// Position among the entity's items.
        index: usize,
    },
    /// The world is part of every map and cannot be hidden.
    CannotHideWorld,
    /// A hidden brush or entity cannot be shown because its block does not parse.
    CannotShow(u32),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchEntity(id) => write!(f, "no entity has id {id}"),
            Self::NoSuchSolid(id) => write!(f, "no brush has id {id}"),
            Self::NoSuchKey { entity, key } => write!(f, "entity {entity} has no key \"{key}\""),
            Self::CannotRemoveWorld => write!(f, "the world cannot be removed"),
            Self::MissingId => write!(f, "the added brush or entity has no id"),
            Self::InvalidResize => write!(
                f,
                "the new size must be larger than zero along every axis the selection spans"
            ),
            Self::InvalidTransform => write!(f, "the transform would flatten the selection"),
            Self::NoSuchBlock { entity, index } => write!(f, "entity {entity} has no block at position {index}"),
            Self::CannotHideWorld => write!(f, "the world cannot be hidden"),
            Self::CannotShow(id) => write!(f, "object {id} is hidden in a form Mallet cannot read"),
        }
    }
}

impl Error for EditError {}

impl Edit {
    /// Applies the edit and returns the edit that undoes it.
    ///
    /// An edit that fails changes nothing: a batch that fails part-way is rolled back
    /// before the error is returned.
    pub fn apply(&self, vmf: &mut Vmf) -> Result<Edit, EditError> {
        match self {
            Self::SetKey { entity, key, value } => {
                let target = entity_mut(vmf, *entity)?;
                let inverse = match target.get(key) {
                    Some(old) => Self::SetKey {
                        entity: *entity,
                        key: key.clone(),
                        value: old.to_owned(),
                    },
                    None => Self::RemoveKey {
                        entity: *entity,
                        key: key.clone(),
                    },
                };
                target.set(key, value.clone());
                Ok(inverse)
            }
            Self::RemoveKey { entity, key } => {
                let target = entity_mut(vmf, *entity)?;
                let old = target.remove(key).ok_or_else(|| EditError::NoSuchKey {
                    entity: *entity,
                    key: key.clone(),
                })?;
                Ok(Self::SetKey {
                    entity: *entity,
                    key: key.clone(),
                    value: old,
                })
            }
            Self::SetConnections { entity, connections } => {
                let target = entity_mut(vmf, *entity)?;
                let previous = target.connections();
                target.set_connections(connections);
                Ok(Self::SetConnections {
                    entity: *entity,
                    connections: previous,
                })
            }
            Self::AddSolid { entity, index, solid } => {
                let id = solid.id().ok_or(EditError::MissingId)?;
                let owner = vmf.entity_position(*entity).ok_or(EditError::NoSuchEntity(*entity))?;
                let target = top_level_entity_mut(vmf, owner);
                let at = index
                    .unwrap_or_else(|| default_solid_index(target))
                    .min(target.items.len());
                target.items.insert(at, EntityItem::Solid(solid.clone()));
                vmf.index.note_solid(id, owner, at);
                Ok(Self::RemoveSolid { solid: id })
            }
            Self::RemoveSolid { solid } => {
                let (entity_index, item_index) = find_solid(vmf, *solid).ok_or(EditError::NoSuchSolid(*solid))?;
                let owner = top_level_entity_mut(vmf, entity_index);
                let entity = owner.id().ok_or(EditError::MissingId)?;
                let EntityItem::Solid(removed) = owner.items.remove(item_index) else {
                    unreachable!("find_solid only returns solid positions");
                };
                Ok(Self::AddSolid {
                    entity,
                    index: Some(item_index),
                    solid: removed,
                })
            }
            Self::AddEntity { index, entity } => {
                let id = entity.id().ok_or(EditError::MissingId)?;
                let at = index.unwrap_or_else(|| default_entity_index(vmf)).min(vmf.items.len());
                vmf.items.insert(at, TopLevel::Entity(entity.clone()));
                vmf.index.note_entity(at, entity);
                Ok(Self::RemoveEntity { entity: id })
            }
            Self::RemoveEntity { entity } => {
                let index = vmf
                    .entity_position(*entity)
                    .filter(|at| matches!(vmf.items[*at], TopLevel::Entity(_)));
                let Some(index) = index else {
                    let is_world = vmf.world().is_some_and(|world| world.id() == Some(*entity));
                    return Err(if is_world {
                        EditError::CannotRemoveWorld
                    } else {
                        EditError::NoSuchEntity(*entity)
                    });
                };
                let TopLevel::Entity(removed) = vmf.items.remove(index) else {
                    unreachable!("position matched an entity");
                };
                vmf.index.note_removed();
                Ok(Self::AddEntity {
                    index: Some(index),
                    entity: removed,
                })
            }
            Self::AddBlock { entity, index, block } => {
                let target = entity_mut(vmf, *entity)?;
                let at = index.unwrap_or(target.items.len()).min(target.items.len());
                target.items.insert(at, EntityItem::Other(block.clone()));
                Ok(Self::RemoveBlock {
                    entity: *entity,
                    index: at,
                })
            }
            Self::RemoveBlock { entity, index } => {
                let target = entity_mut(vmf, *entity)?;
                let missing = EditError::NoSuchBlock {
                    entity: *entity,
                    index: *index,
                };
                if !matches!(target.items.get(*index), Some(EntityItem::Other(_))) {
                    return Err(missing);
                }
                let EntityItem::Other(block) = target.items.remove(*index) else {
                    return Err(missing);
                };
                Ok(Self::AddBlock {
                    entity: *entity,
                    index: Some(*index),
                    block,
                })
            }
            Self::Translate { solids, offset } => {
                if let Some(missing) = solids.iter().find(|id| find_solid(vmf, **id).is_none()) {
                    return Err(EditError::NoSuchSolid(*missing));
                }
                for id in solids {
                    if let Some(solid) = solid_mut(vmf, *id) {
                        solid.translate(*offset);
                    }
                }
                Ok(Self::Translate {
                    solids: solids.clone(),
                    offset: -*offset,
                })
            }
            Self::Rescale { solids, from, to } => {
                let flattened = (0..3).any(|axis| from.size()[axis] > 0.0 && to.size()[axis] <= 0.0);
                if flattened || from.is_empty() || to.is_empty() {
                    return Err(EditError::InvalidResize);
                }
                let originals = solids
                    .iter()
                    .map(|id| {
                        solid_mut(vmf, *id)
                            .map(|solid| solid.clone())
                            .ok_or(EditError::NoSuchSolid(*id))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let (from, to) = (*from, *to);
                for id in solids {
                    if let Some(solid) = solid_mut(vmf, *id) {
                        solid.transform(&|point| from.remap(&to, point));
                    }
                }
                Ok(Self::ReplaceSolids(originals))
            }
            Self::Transform { solids, matrix, lock } => {
                if !transform::is_invertible(matrix) {
                    return Err(EditError::InvalidTransform);
                }
                let originals = solids
                    .iter()
                    .map(|id| {
                        solid_mut(vmf, *id)
                            .map(|solid| solid.clone())
                            .ok_or(EditError::NoSuchSolid(*id))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                for id in solids {
                    if let Some(solid) = solid_mut(vmf, *id) {
                        solid.apply_matrix(matrix, *lock);
                    }
                }
                Ok(Self::ReplaceSolids(originals))
            }
            Self::ReplaceSolids(replacements) => {
                let ids = replacements
                    .iter()
                    .map(|solid| solid.id().ok_or(EditError::MissingId))
                    .collect::<Result<Vec<_>, _>>()?;
                if let Some(missing) = ids.iter().find(|id| find_solid(vmf, **id).is_none()) {
                    return Err(EditError::NoSuchSolid(*missing));
                }
                let mut previous = Vec::with_capacity(replacements.len());
                for (id, replacement) in ids.iter().zip(replacements) {
                    if let Some(solid) = solid_mut(vmf, *id) {
                        previous.push(std::mem::replace(solid, replacement.clone()));
                    }
                }
                Ok(Self::ReplaceSolids(previous))
            }
            Self::SetHidden {
                solids,
                entities,
                hidden,
            } => {
                let inverse = set_hidden(vmf, solids, entities, *hidden);
                vmf.forget_positions();
                inverse
            }
            Self::SetEditors { solids, entities } => set_editors(vmf, solids, entities),
            Self::SetVisGroups(block) => {
                let position = vmf.items.iter().position(
                    |item| matches!(item, TopLevel::Other(block) if block.name.eq_ignore_ascii_case("visgroups")),
                );
                let previous = match (position, block) {
                    (Some(index), Some(block)) => {
                        match std::mem::replace(&mut vmf.items[index], TopLevel::Other(block.clone())) {
                            TopLevel::Other(old) => Some(old),
                            _ => None,
                        }
                    }
                    (Some(index), None) => match vmf.items.remove(index) {
                        TopLevel::Other(old) => Some(old),
                        _ => None,
                    },
                    (None, Some(block)) => {
                        let at = vmf
                            .items
                            .iter()
                            .position(
                                |item| matches!(item, TopLevel::Other(block) if block.name.eq_ignore_ascii_case("versioninfo")),
                            )
                            .map_or(0, |index| index + 1);
                        vmf.items.insert(at, TopLevel::Other(block.clone()));
                        None
                    }
                    (None, None) => None,
                };
                Ok(Self::SetVisGroups(previous))
            }
            Self::Batch(edits) => {
                let mut inverses = Vec::with_capacity(edits.len());
                for edit in edits {
                    match edit.apply(vmf) {
                        Ok(inverse) => inverses.push(inverse),
                        Err(error) => {
                            for inverse in inverses.iter().rev() {
                                let _ = inverse.apply(vmf);
                            }
                            return Err(error);
                        }
                    }
                }
                inverses.reverse();
                Ok(Self::Batch(inverses))
            }
        }
    }
}

fn entity_mut(vmf: &mut Vmf, id: u32) -> Result<&mut Entity, EditError> {
    vmf.entity_by_id_mut(id).ok_or(EditError::NoSuchEntity(id))
}

fn top_level_entity_mut(vmf: &mut Vmf, index: usize) -> &mut Entity {
    match &mut vmf.items[index] {
        TopLevel::World(entity) | TopLevel::Entity(entity) => entity,
        _ => unreachable!("find_solid only returns entity positions"),
    }
}

/// Finds a brush by id: the index of its owner in the map's items and its index inside
/// the owner.
fn find_solid(vmf: &Vmf, id: u32) -> Option<(usize, usize)> {
    vmf.solid_position(id)
}

fn solid_mut(vmf: &mut Vmf, id: u32) -> Option<&mut Solid> {
    let (entity_index, item_index) = find_solid(vmf, id)?;
    match &mut top_level_entity_mut(vmf, entity_index).items[item_index] {
        EntityItem::Solid(solid) => Some(solid),
        _ => None,
    }
}

/// The ids of every hidden brush and entity, brushes first. Hidden objects are invisible
/// to [`Vmf::all_entities`] and to the editor, so this is how they are found again.
pub fn hidden_ids(vmf: &Vmf) -> (Vec<u32>, Vec<u32>) {
    let mut solids = Vec::new();
    let mut entities = Vec::new();
    for item in &vmf.items {
        match item {
            TopLevel::World(entity) | TopLevel::Entity(entity) => {
                for item in &entity.items {
                    if let EntityItem::Other(block) = item {
                        if is_hidden_block(block) {
                            solids.extend(block.blocks().filter_map(|child| child_id(child, "solid")));
                        }
                    }
                }
            }
            TopLevel::Other(block) if is_hidden_block(block) => {
                entities.extend(block.blocks().filter_map(|child| child_id(child, "entity")));
            }
            _ => {}
        }
    }
    (solids, entities)
}

/// Where a brush or an entity sits in the map.
#[derive(Clone, Copy)]
struct Place {
    /// Position among the map's top-level items: the entity itself, or a brush's owner.
    top: usize,
    /// A brush's position among its owner's items.
    item: usize,
    /// Whether the object is wrapped in a `hidden` block.
    hidden: bool,
}

/// Every brush and entity of a map by id, hidden ones included, so edits that touch many
/// objects look each one up once. A visible object wins over a hidden one with its id.
struct ObjectIndex {
    solids: HashMap<u32, Place>,
    entities: HashMap<u32, Place>,
}

impl ObjectIndex {
    fn new(vmf: &Vmf) -> Self {
        let mut index = Self {
            solids: HashMap::new(),
            entities: HashMap::new(),
        };
        for (top, item) in vmf.items.iter().enumerate() {
            match item {
                TopLevel::World(entity) | TopLevel::Entity(entity) => {
                    if let (TopLevel::Entity(_), Some(id)) = (item, entity.id()) {
                        Self::add(&mut index.entities, id, Place { top, item: 0, hidden: false });
                    }
                    for (position, item) in entity.items.iter().enumerate() {
                        match item {
                            EntityItem::Solid(solid) => {
                                if let Some(id) = solid.id() {
                                    let place = Place { top, item: position, hidden: false };
                                    Self::add(&mut index.solids, id, place);
                                }
                            }
                            EntityItem::Other(block) if is_hidden_block(block) => {
                                for id in block.blocks().filter_map(|child| child_id(child, "solid")) {
                                    let place = Place { top, item: position, hidden: true };
                                    Self::add(&mut index.solids, id, place);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                TopLevel::Other(block) if is_hidden_block(block) => {
                    for id in block.blocks().filter_map(|child| child_id(child, "entity")) {
                        Self::add(&mut index.entities, id, Place { top, item: 0, hidden: true });
                    }
                }
                _ => {}
            }
        }
        index
    }

    fn add(places: &mut HashMap<u32, Place>, id: u32, place: Place) {
        let known = places.entry(id).or_insert(place);
        if known.hidden && !place.hidden {
            *known = place;
        }
    }
}

/// Hides or shows the given objects, checking all of them before changing any so that a
/// failing edit leaves the map alone. Returns the edit that undoes it, naming only the
/// objects that actually changed.
fn set_hidden(vmf: &mut Vmf, solids: &[u32], entities: &[u32], hidden: bool) -> Result<Edit, EditError> {
    let world_id = vmf.world().and_then(Entity::id);
    let index = ObjectIndex::new(vmf);
    let mut seen = HashSet::new();
    let mut changed_solids = Vec::new();
    for &id in solids {
        let place = *index.solids.get(&id).ok_or(EditError::NoSuchSolid(id))?;
        if place.hidden == hidden || !seen.insert(id) {
            continue;
        }
        let shown = if hidden {
            None
        } else {
            let solid = hidden_solid(vmf, place, id)
                .and_then(|block| Solid::from_block(block.clone()).ok())
                .ok_or(EditError::CannotShow(id))?;
            Some(solid)
        };
        changed_solids.push((id, place, shown));
    }
    seen.clear();
    let mut changed_entities = Vec::new();
    for &id in entities {
        if world_id == Some(id) {
            if hidden {
                return Err(EditError::CannotHideWorld);
            }
            continue;
        }
        let place = *index.entities.get(&id).ok_or(EditError::NoSuchEntity(id))?;
        if place.hidden == hidden || !seen.insert(id) {
            continue;
        }
        let shown = if hidden {
            None
        } else {
            let entity = hidden_entity(vmf, place, id)
                .and_then(|block| Entity::from_block(block.clone()).ok())
                .ok_or(EditError::CannotShow(id))?;
            Some(entity)
        };
        changed_entities.push((id, place, shown));
    }
    let solid_ids = changed_solids.iter().map(|(id, _, _)| *id).collect();
    let entity_ids = changed_entities.iter().map(|(id, _, _)| *id).collect();
    for (_, place, shown) in changed_solids {
        let slot = &mut top_level_entity_mut(vmf, place.top).items[place.item];
        *slot = match (shown, std::mem::replace(slot, EntityItem::Other(Block::default()))) {
            (Some(solid), _) => EntityItem::Solid(solid),
            (None, EntityItem::Solid(solid)) => EntityItem::Other(hidden_wrapper(solid.to_block())),
            (None, other) => other,
        };
    }
    for (_, place, shown) in changed_entities {
        let slot = &mut vmf.items[place.top];
        *slot = match (shown, std::mem::replace(slot, TopLevel::Other(Block::default()))) {
            (Some(entity), _) => TopLevel::Entity(entity),
            (None, TopLevel::Entity(entity)) => TopLevel::Other(hidden_wrapper(entity.to_block("entity"))),
            (None, other) => other,
        };
    }
    Ok(Edit::SetHidden {
        solids: solid_ids,
        entities: entity_ids,
        hidden: !hidden,
    })
}

/// Replaces the `editor` blocks of the given objects, checking that all of them exist
/// before changing any. Returns the edit that puts the old blocks back.
fn set_editors(
    vmf: &mut Vmf,
    solids: &[(u32, Option<Block>)],
    entities: &[(u32, Option<Block>)],
) -> Result<Edit, EditError> {
    let index = ObjectIndex::new(vmf);
    if let Some((id, _)) = solids.iter().find(|(id, _)| !index.solids.contains_key(id)) {
        return Err(EditError::NoSuchSolid(*id));
    }
    if let Some((id, _)) = entities.iter().find(|(id, _)| !index.entities.contains_key(id)) {
        return Err(EditError::NoSuchEntity(*id));
    }
    let mut old_solids = Vec::with_capacity(solids.len());
    for (id, editor) in solids {
        let place = index.solids[id];
        let previous = match &mut top_level_entity_mut(vmf, place.top).items[place.item] {
            EntityItem::Solid(solid) => swap_solid_editor(solid, editor.clone()),
            EntityItem::Other(wrapper) => {
                wrapped_child_mut(wrapper, "solid", *id).and_then(|child| swap_block_editor(child, editor.clone()))
            }
            EntityItem::Key(_) => None,
        };
        old_solids.push((*id, previous));
    }
    let mut old_entities = Vec::with_capacity(entities.len());
    for (id, editor) in entities {
        let previous = match &mut vmf.items[index.entities[id].top] {
            TopLevel::Entity(entity) => swap_entity_editor(entity, editor.clone()),
            TopLevel::Other(wrapper) => {
                wrapped_child_mut(wrapper, "entity", *id).and_then(|child| swap_block_editor(child, editor.clone()))
            }
            _ => None,
        };
        old_entities.push((*id, previous));
    }
    old_solids.reverse();
    old_entities.reverse();
    Ok(Edit::SetEditors {
        solids: old_solids,
        entities: old_entities,
    })
}

fn is_editor_block(block: &Block) -> bool {
    block.name.eq_ignore_ascii_case("editor")
}

/// Puts `editor` in place of a brush's `editor` block, or at its end when it has none,
/// and returns the old block.
fn swap_solid_editor(solid: &mut Solid, editor: Option<Block>) -> Option<Block> {
    let position = solid
        .items
        .iter()
        .position(|item| matches!(item, SolidItem::Other(block) if is_editor_block(block)));
    match (position, editor) {
        (Some(index), Some(editor)) => match std::mem::replace(&mut solid.items[index], SolidItem::Other(editor)) {
            SolidItem::Other(old) => Some(old),
            _ => None,
        },
        (Some(index), None) => match solid.items.remove(index) {
            SolidItem::Other(old) => Some(old),
            _ => None,
        },
        (None, Some(editor)) => {
            solid.items.push(SolidItem::Other(editor));
            None
        }
        (None, None) => None,
    }
}

/// Puts `editor` in place of an entity's `editor` block, or at its end when it has none,
/// and returns the old block.
fn swap_entity_editor(entity: &mut Entity, editor: Option<Block>) -> Option<Block> {
    let position = entity
        .items
        .iter()
        .position(|item| matches!(item, EntityItem::Other(block) if is_editor_block(block)));
    match (position, editor) {
        (Some(index), Some(editor)) => match std::mem::replace(&mut entity.items[index], EntityItem::Other(editor)) {
            EntityItem::Other(old) => Some(old),
            _ => None,
        },
        (Some(index), None) => match entity.items.remove(index) {
            EntityItem::Other(old) => Some(old),
            _ => None,
        },
        (None, Some(editor)) => {
            entity.items.push(EntityItem::Other(editor));
            None
        }
        (None, None) => None,
    }
}

/// Puts `editor` in place of the `editor` block of a brush or entity that is kept as a
/// plain block while hidden, or at its end when it has none, and returns the old block.
fn swap_block_editor(object: &mut Block, editor: Option<Block>) -> Option<Block> {
    let position = object
        .items
        .iter()
        .position(|item| matches!(item, Item::Block(block) if is_editor_block(block)));
    match (position, editor) {
        (Some(index), Some(editor)) => match std::mem::replace(&mut object.items[index], Item::Block(editor)) {
            Item::Block(old) => Some(old),
            Item::Pair(_) => None,
        },
        (Some(index), None) => match object.items.remove(index) {
            Item::Block(old) => Some(old),
            Item::Pair(_) => None,
        },
        (None, Some(editor)) => {
            object.items.push(Item::Block(editor));
            None
        }
        (None, None) => None,
    }
}

/// A `hidden` block around one brush or entity block.
fn hidden_wrapper(child: Block) -> Block {
    let mut wrapper = Block::new("hidden");
    wrapper.items.push(Item::Block(child));
    wrapper
}

pub(crate) fn is_hidden_block(block: &Block) -> bool {
    block.name.eq_ignore_ascii_case("hidden")
}

/// The numeric `id` of a child block, when it has one of the given name.
pub(crate) fn child_id(block: &Block, name: &str) -> Option<u32> {
    if !block.name.eq_ignore_ascii_case(name) {
        return None;
    }
    block.get("id")?.trim().parse().ok()
}

/// The child of kind `name` with the given id inside a `hidden` block.
fn wrapped_child<'a>(wrapper: &'a Block, name: &str, id: u32) -> Option<&'a Block> {
    wrapper.blocks().find(|child| child_id(child, name) == Some(id))
}

fn wrapped_child_mut<'a>(wrapper: &'a mut Block, name: &str, id: u32) -> Option<&'a mut Block> {
    wrapper.items.iter_mut().find_map(|item| match item {
        Item::Block(child) if child_id(child, name) == Some(id) => Some(child),
        _ => None,
    })
}

/// The block of a hidden brush, found through its place in the map.
fn hidden_solid(vmf: &Vmf, place: Place, id: u32) -> Option<&Block> {
    let (TopLevel::World(entity) | TopLevel::Entity(entity)) = &vmf.items[place.top] else {
        return None;
    };
    match &entity.items[place.item] {
        EntityItem::Other(wrapper) => wrapped_child(wrapper, "solid", id),
        _ => None,
    }
}

/// The block of a hidden entity, found through its place in the map.
fn hidden_entity(vmf: &Vmf, place: Place, id: u32) -> Option<&Block> {
    match &vmf.items[place.top] {
        TopLevel::Other(wrapper) => wrapped_child(wrapper, "entity", id),
        _ => None,
    }
}

/// Just after the entity's last brush, or after its keys when it has none.
fn default_solid_index(entity: &Entity) -> usize {
    entity
        .items
        .iter()
        .rposition(|item| matches!(item, EntityItem::Solid(_)))
        .or_else(|| entity.items.iter().rposition(|item| matches!(item, EntityItem::Key(_))))
        .map_or(0, |index| index + 1)
}

/// Just after the map's last entity or world block.
fn default_entity_index(vmf: &Vmf) -> usize {
    vmf.items
        .iter()
        .rposition(|item| matches!(item, TopLevel::World(_) | TopLevel::Entity(_)))
        .map_or(vmf.items.len(), |index| index + 1)
}

/// Undo and redo stacks of applied edits.
#[derive(Debug, Clone, Default)]
pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// The group of the latest edit made with [`History::apply_grouped`], while nothing
    /// else has happened since.
    last_group: Option<u64>,
}

impl History {
    /// Applies an edit, records it for undo and clears the redo stack.
    pub fn apply(&mut self, vmf: &mut Vmf, edit: &Edit) -> Result<(), EditError> {
        let inverse = edit.apply(vmf)?;
        self.undo.push(inverse);
        self.redo.clear();
        self.last_group = None;
        Ok(())
    }

    /// Applies an edit that continues the previous one when both have the same `group`,
    /// such as each step of dragging a value: they undo together, back to before the
    /// first. The edits of one group must change the same things, so the first inverse
    /// restores all of them, except that steps replacing brushes may reach new ones, as
    /// a paint stroke does: a brush a later step changes first joins the first inverse.
    pub fn apply_grouped(&mut self, vmf: &mut Vmf, edit: &Edit, group: u64) -> Result<(), EditError> {
        let inverse = edit.apply(vmf)?;
        if self.last_group != Some(group) || self.undo.is_empty() {
            self.undo.push(inverse);
        } else if let (Some(Edit::ReplaceSolids(first)), Edit::ReplaceSolids(later)) = (self.undo.last_mut(), inverse) {
            for solid in later {
                if !first.iter().any(|kept| kept.id() == solid.id()) {
                    first.push(solid);
                }
            }
        }
        self.redo.clear();
        self.last_group = Some(group);
        Ok(())
    }

    /// Joins the latest `count` undo steps into one, so a single undo reverts them all,
    /// latest first. Does nothing when fewer steps are recorded.
    pub fn join_latest(&mut self, count: usize) {
        if count < 2 || self.undo.len() < count {
            return;
        }
        let joined: Vec<Edit> = self.undo.drain(self.undo.len() - count..).rev().collect();
        self.undo.push(Edit::Batch(joined));
        self.last_group = None;
    }

    /// Undoes the latest edit. Returns `false` when there is nothing to undo.
    pub fn undo(&mut self, vmf: &mut Vmf) -> Result<bool, EditError> {
        self.last_group = None;
        let Some(edit) = self.undo.pop() else {
            return Ok(false);
        };
        let inverse = edit.apply(vmf)?;
        self.redo.push(inverse);
        Ok(true)
    }

    /// Redoes the latest undone edit. Returns `false` when there is nothing to redo.
    pub fn redo(&mut self, vmf: &mut Vmf) -> Result<bool, EditError> {
        self.last_group = None;
        let Some(edit) = self.redo.pop() else {
            return Ok(false);
        };
        let inverse = edit.apply(vmf)?;
        self.undo.push(inverse);
        Ok(true)
    }

    /// Whether an undo step is available.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether a redo step is available.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = r#"
world { "id" "1" "classname" "worldspawn" }
entity
{
	"id" "2"
	"classname" "logic_relay"
	"targetname" "relay"
	connections
	{
		"OnTrigger" "door,Open,,0,-1"
		"OnTrigger" "lamp,SetColor,255 0 0,1.5,1"
	}
}
entity { "id" "3" "classname" "info_target" }
"#;

    fn entity(vmf: &Vmf, id: u32) -> &Entity {
        vmf.entities().find(|entity| entity.id() == Some(id)).unwrap()
    }

    #[test]
    fn grouped_edits_undo_together() {
        let mut vmf = Vmf::parse(MAP).unwrap();
        let original = vmf.to_text();
        let mut history = History::default();
        let set = |value: &str| Edit::SetKey {
            entity: 2,
            key: "targetname".to_owned(),
            value: value.to_owned(),
        };
        for value in ["a", "ab", "abc"] {
            history.apply_grouped(&mut vmf, &set(value), 7).unwrap();
        }
        history.apply_grouped(&mut vmf, &set("other"), 8).unwrap();
        assert!(history.undo(&mut vmf).unwrap());
        assert_eq!(entity(&vmf, 2).get("targetname"), Some("abc"));
        assert!(history.undo(&mut vmf).unwrap());
        assert_eq!(vmf.to_text(), original, "the three steps of group 7 undo as one");
        assert!(!history.undo(&mut vmf).unwrap());
    }

    #[test]
    fn a_grouped_stroke_undoes_every_brush_it_reached() {
        let mut vmf = Vmf::new_map();
        let mut ids = crate::IdAllocator::for_map(&vmf);
        let mut history = History::default();
        let mut solids = Vec::new();
        for x in [0.0, 128.0] {
            let solid = crate::build::box_solid(
                DVec3::new(x, 0.0, 0.0),
                DVec3::new(x + 64.0, 64.0, 64.0),
                "BRICK/BRICKWALL001A",
                &mut ids,
            )
            .unwrap();
            history
                .apply(
                    &mut vmf,
                    &Edit::AddSolid {
                        entity: 1,
                        index: None,
                        solid: solid.clone(),
                    },
                )
                .unwrap();
            solids.push(solid);
        }
        let original = vmf.to_text();
        let retextured = |solid: &Solid, material: &str| {
            let mut solid = solid.clone();
            for side in solid.sides_mut() {
                side.material = material.to_owned();
            }
            solid
        };
        let steps = [
            vec![retextured(&solids[0], "A")],
            vec![retextured(&solids[0], "B"), retextured(&solids[1], "B")],
        ];
        for step in steps {
            history.apply_grouped(&mut vmf, &Edit::ReplaceSolids(step), 3).unwrap();
        }
        assert!(history.undo(&mut vmf).unwrap());
        assert_eq!(
            vmf.to_text(),
            original,
            "the second brush, first reached by the second step, is restored too"
        );
    }

    #[test]
    fn reads_connections() {
        let vmf = Vmf::parse(MAP).unwrap();
        let connections = entity(&vmf, 2).connections();
        assert_eq!(connections.len(), 2);
        assert_eq!(connections[1].parameter, "255 0 0");
        assert_eq!(connections[1].delay, 1.5);
        assert_eq!(connections[1].refires, 1);
        assert!(entity(&vmf, 3).connections().is_empty());
    }

    #[test]
    fn setting_connections_undoes_to_the_exact_text() {
        let mut vmf = Vmf::parse(MAP).unwrap();
        let original = vmf.to_text();
        let mut history = History::default();
        let mut connections = entity(&vmf, 2).connections();
        connections.remove(0);
        history
            .apply(&mut vmf, &Edit::SetConnections { entity: 2, connections })
            .unwrap();
        assert_eq!(entity(&vmf, 2).connections().len(), 1);

        let added = Connection {
            output: "OnUser1".to_owned(),
            target: "relay".to_owned(),
            input: "Trigger".to_owned(),
            parameter: String::new(),
            delay: 0.0,
            refires: -1,
        };
        history
            .apply(
                &mut vmf,
                &Edit::SetConnections {
                    entity: 3,
                    connections: vec![added.clone()],
                },
            )
            .unwrap();
        assert_eq!(entity(&vmf, 3).connections(), [added]);
        assert!(vmf.to_text().contains("\"OnUser1\" \"relay,Trigger,,0,-1\""));

        assert!(history.undo(&mut vmf).unwrap());
        assert!(history.undo(&mut vmf).unwrap());
        assert_eq!(vmf.to_text(), original);
    }

    #[test]
    fn hiding_wraps_objects_in_hidden_blocks_and_showing_restores_them() {
        let mut vmf = Vmf::parse(MAP).unwrap();
        let mut ids = crate::IdAllocator::for_map(&vmf);
        let brush = crate::build::box_solid(DVec3::ZERO, DVec3::splat(64.0), "DEV/DEV_MEASUREGENERIC01B", &mut ids)
            .unwrap();
        let brush_id = brush.id().unwrap();
        let mut history = History::default();
        history
            .apply(&mut vmf, &Edit::AddSolid { entity: 1, index: None, solid: brush })
            .unwrap();
        let visible = vmf.to_text();

        let hide = Edit::SetHidden {
            solids: vec![brush_id],
            entities: vec![3],
            hidden: true,
        };
        history.apply(&mut vmf, &hide).unwrap();
        assert!(find_solid(&vmf, brush_id).is_none(), "a hidden brush leaves the map's solids");
        assert!(vmf.entities().all(|entity| entity.id() != Some(3)));
        assert_eq!(hidden_ids(&vmf), (vec![brush_id], vec![3]));
        assert!(vmf.to_text().contains("hidden"), "hidden objects are written in hidden blocks");
        let again = hide.apply(&mut vmf).unwrap();
        assert_eq!(
            again,
            Edit::SetHidden {
                solids: vec![],
                entities: vec![],
                hidden: false,
            }
        );

        assert!(history.undo(&mut vmf).unwrap());
        assert_eq!(vmf.to_text(), visible, "showing again restores the brush and entity");
        assert_eq!(hidden_ids(&vmf), (vec![], vec![]));
        assert!(history.undo(&mut vmf).unwrap());
        assert_eq!(hidden_ids(&vmf), (vec![], vec![]));
    }

    #[test]
    fn hiding_the_world_fails_and_changes_nothing() {
        let mut vmf = Vmf::parse(MAP).unwrap();
        let original = vmf.to_text();
        let error = Edit::SetHidden {
            solids: vec![],
            entities: vec![2, 1],
            hidden: true,
        }
        .apply(&mut vmf)
        .unwrap_err();
        assert_eq!(error, EditError::CannotHideWorld);
        assert_eq!(vmf.to_text(), original);
    }
}
