//! Reversible edits and an undo history.
//!
//! Every change to a map, whether it comes from the editor UI or from an automation client
//! such as an MCP server, is expressed as an [`Edit`]. Applying an edit returns its inverse,
//! which is what makes undo and redo work the same way for every caller.

use std::error::Error;
use std::fmt;

use glam::{DMat4, DVec3};

use crate::geometry::Bounds;
use crate::kv::{Block, Item};
use crate::transform::{self, TextureLock};
use crate::{Connection, Entity, EntityItem, Solid, TopLevel, Vmf};

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
                let target = entity_mut(vmf, *entity)?;
                let at = index
                    .unwrap_or_else(|| default_solid_index(target))
                    .min(target.items.len());
                target.items.insert(at, EntityItem::Solid(solid.clone()));
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
                Ok(Self::RemoveEntity { entity: id })
            }
            Self::RemoveEntity { entity } => {
                let index = vmf
                    .items
                    .iter()
                    .position(|item| matches!(item, TopLevel::Entity(e) if e.id() == Some(*entity)));
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
            } => set_hidden(vmf, solids, entities, *hidden),
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
    vmf.items
        .iter_mut()
        .find_map(|item| match item {
            TopLevel::World(entity) | TopLevel::Entity(entity) if entity.id() == Some(id) => Some(entity),
            _ => None,
        })
        .ok_or(EditError::NoSuchEntity(id))
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
    vmf.items.iter().enumerate().find_map(|(entity_index, item)| {
        let (TopLevel::World(entity) | TopLevel::Entity(entity)) = item else {
            return None;
        };
        entity
            .items
            .iter()
            .position(|item| matches!(item, EntityItem::Solid(solid) if solid.id() == Some(id)))
            .map(|item_index| (entity_index, item_index))
    })
}

fn solid_mut(vmf: &mut Vmf, id: u32) -> Option<&mut Solid> {
    let (entity_index, item_index) = find_solid(vmf, id)?;
    match &mut top_level_entity_mut(vmf, entity_index).items[item_index] {
        EntityItem::Solid(solid) => Some(solid),
        _ => None,
    }
}

/// Just after the entity's last brush, or after its keys when it has none.
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

/// Hides or shows the given objects, checking all of them before changing any so that a
/// failing edit leaves the map alone. Returns the edit that undoes it, naming only the
/// objects that actually changed.
fn set_hidden(vmf: &mut Vmf, solids: &[u32], entities: &[u32], hidden: bool) -> Result<Edit, EditError> {
    let world_id = vmf.world().and_then(Entity::id);
    let mut changed_solids = Vec::new();
    for &id in solids {
        let visible = find_solid(vmf, id).is_some();
        let hidden_now = find_hidden_solid(vmf, id).is_some();
        if !visible && !hidden_now {
            return Err(EditError::NoSuchSolid(id));
        }
        if hidden_now == hidden {
            continue;
        }
        if !hidden && hidden_solid(vmf, id).and_then(|block| Solid::from_block(block).ok()).is_none() {
            return Err(EditError::CannotShow(id));
        }
        changed_solids.push(id);
    }
    let mut changed_entities = Vec::new();
    for &id in entities {
        if world_id == Some(id) {
            if hidden {
                return Err(EditError::CannotHideWorld);
            }
            continue;
        }
        let visible = find_entity_position(vmf, id).is_some();
        let hidden_now = find_hidden_entity(vmf, id).is_some();
        if !visible && !hidden_now {
            return Err(EditError::NoSuchEntity(id));
        }
        if hidden_now == hidden {
            continue;
        }
        if !hidden && hidden_entity(vmf, id).and_then(|block| Entity::from_block(block).ok()).is_none() {
            return Err(EditError::CannotShow(id));
        }
        changed_entities.push(id);
    }
    for &id in &changed_solids {
        if hidden {
            let (entity_index, item_index) = find_solid(vmf, id).ok_or(EditError::NoSuchSolid(id))?;
            let entity = top_level_entity_mut(vmf, entity_index);
            let EntityItem::Solid(solid) = entity.items.remove(item_index) else {
                unreachable!("find_solid only returns solid positions");
            };
            entity
                .items
                .insert(item_index, EntityItem::Other(hidden_wrapper(solid.to_block())));
        } else {
            let (entity_index, item_index) = find_hidden_solid(vmf, id).ok_or(EditError::NoSuchSolid(id))?;
            let entity = top_level_entity_mut(vmf, entity_index);
            let EntityItem::Other(wrapper) = entity.items.remove(item_index) else {
                unreachable!("find_hidden_solid only returns hidden block positions");
            };
            let child = unwrap_hidden(wrapper, "solid", id).ok_or(EditError::NoSuchSolid(id))?;
            let solid = Solid::from_block(child).map_err(|_| EditError::CannotShow(id))?;
            entity.items.insert(item_index, EntityItem::Solid(solid));
        }
    }
    for &id in &changed_entities {
        if hidden {
            let index = find_entity_position(vmf, id).ok_or(EditError::NoSuchEntity(id))?;
            let TopLevel::Entity(entity) = vmf.items.remove(index) else {
                unreachable!("find_entity_position only returns entity positions");
            };
            vmf.items
                .insert(index, TopLevel::Other(hidden_wrapper(entity.to_block("entity"))));
        } else {
            let index = find_hidden_entity(vmf, id).ok_or(EditError::NoSuchEntity(id))?;
            let TopLevel::Other(wrapper) = vmf.items.remove(index) else {
                unreachable!("find_hidden_entity only returns hidden block positions");
            };
            let child = unwrap_hidden(wrapper, "entity", id).ok_or(EditError::NoSuchEntity(id))?;
            let entity = Entity::from_block(child).map_err(|_| EditError::CannotShow(id))?;
            vmf.items.insert(index, TopLevel::Entity(entity));
        }
    }
    Ok(Edit::SetHidden {
        solids: changed_solids,
        entities: changed_entities,
        hidden: !hidden,
    })
}

/// A `hidden` block around one brush or entity block.
fn hidden_wrapper(child: Block) -> Block {
    let mut wrapper = Block::new("hidden");
    wrapper.items.push(Item::Block(child));
    wrapper
}

fn is_hidden_block(block: &Block) -> bool {
    block.name.eq_ignore_ascii_case("hidden")
}

/// The numeric `id` of a child block, when it has one of the given name.
fn child_id(block: &Block, name: &str) -> Option<u32> {
    if !block.name.eq_ignore_ascii_case(name) {
        return None;
    }
    block.get("id")?.trim().parse().ok()
}

/// Takes the child of kind `name` with the given id out of a `hidden` block.
fn unwrap_hidden(wrapper: Block, name: &str, id: u32) -> Option<Block> {
    wrapper.items.into_iter().find_map(|item| match item {
        Item::Block(child) if child_id(&child, name) == Some(id) => Some(child),
        _ => None,
    })
}

/// Finds a hidden brush: the top-level entity that holds it, and the position of its
/// `hidden` block among that entity's items.
fn find_hidden_solid(vmf: &Vmf, id: u32) -> Option<(usize, usize)> {
    vmf.items.iter().enumerate().find_map(|(entity_index, item)| {
        let (TopLevel::World(entity) | TopLevel::Entity(entity)) = item else {
            return None;
        };
        entity
            .items
            .iter()
            .position(|item| match item {
                EntityItem::Other(block) => is_hidden_block(block) && hidden_child_matches(block, "solid", id),
                _ => false,
            })
            .map(|item_index| (entity_index, item_index))
    })
}

/// A copy of a hidden brush's block, for checking that it reads back.
fn hidden_solid(vmf: &Vmf, id: u32) -> Option<Block> {
    let (entity_index, item_index) = find_hidden_solid(vmf, id)?;
    let (TopLevel::World(entity) | TopLevel::Entity(entity)) = &vmf.items[entity_index] else {
        return None;
    };
    match &entity.items[item_index] {
        EntityItem::Other(wrapper) => unwrap_hidden(wrapper.clone(), "solid", id),
        _ => None,
    }
}

/// The position of a visible entity among the map's top-level items.
fn find_entity_position(vmf: &Vmf, id: u32) -> Option<usize> {
    vmf.items
        .iter()
        .position(|item| matches!(item, TopLevel::Entity(entity) if entity.id() == Some(id)))
}

/// The position of a hidden entity's `hidden` block among the map's top-level items.
fn find_hidden_entity(vmf: &Vmf, id: u32) -> Option<usize> {
    vmf.items.iter().position(|item| match item {
        TopLevel::Other(block) => is_hidden_block(block) && hidden_child_matches(block, "entity", id),
        _ => false,
    })
}

/// A copy of a hidden entity's block, for checking that it reads back.
fn hidden_entity(vmf: &Vmf, id: u32) -> Option<Block> {
    match &vmf.items[find_hidden_entity(vmf, id)?] {
        TopLevel::Other(wrapper) => unwrap_hidden(wrapper.clone(), "entity", id),
        _ => None,
    }
}

fn hidden_child_matches(wrapper: &Block, name: &str, id: u32) -> bool {
    wrapper.blocks().any(|child| child_id(child, name) == Some(id))
}

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
    /// restores all of them.
    pub fn apply_grouped(&mut self, vmf: &mut Vmf, edit: &Edit, group: u64) -> Result<(), EditError> {
        let inverse = edit.apply(vmf)?;
        if self.last_group != Some(group) || self.undo.is_empty() {
            self.undo.push(inverse);
        }
        self.redo.clear();
        self.last_group = Some(group);
        Ok(())
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
        // Hiding again changes nothing, so its undo names no objects.
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
