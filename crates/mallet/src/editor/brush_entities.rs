//! Hammer's Tie to Entity (Ctrl+T) and Move to World (Ctrl+Shift+W): turning world
//! brushes into a brush entity such as `func_detail` or `func_door`, and back.

use mallet_content::ClassKind;
use mallet_vmf::edit::Edit;
use mallet_vmf::{Entity, EntityItem, IdAllocator, Solid};

use super::{find_entity, Editor, SelectMode};
use crate::scene::Owner;

/// The class a brush entity gets when it is made with Tie to Entity, as Hammer's game
/// configurations set it (`DefaultSolidEntity`).
pub const DEFAULT_SOLID_ENTITY: &str = "func_detail";

impl Editor {
    /// The brush entities in the selection, which Tie to Entity can add the other selected
    /// brushes to instead of making a new entity: their ids and class names, oldest first.
    pub fn selected_brush_entities(&self) -> Vec<(u32, String)> {
        self.selection
            .iter()
            .filter_map(|owner| match owner {
                Owner::Entity(id) => self.entity(*id),
                Owner::Solid(_) => None,
            })
            .filter(|entity| entity.solids().next().is_some())
            .filter_map(|entity| Some((entity.id()?, entity.classname().to_owned())))
            .collect()
    }

    /// Ties the selected world brushes, and the brushes of selected brush entities, to a
    /// new entity of `classname` with the class's default keyvalues, and selects it.
    /// The brush entities whose brushes moved are removed. One undo step. Returns the new
    /// entity's id.
    pub fn tie_to_new_entity(&mut self, classname: &str) -> Result<u32, String> {
        let classname = classname.trim();
        if classname.is_empty() {
            return Err("choose a class for the new entity".to_owned());
        }
        let (solids, emptied) = self.solids_to_tie(None)?;
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let id = IdAllocator::for_map(vmf).object();
        let mut entity = Entity::default();
        entity.set("id", id.to_string());
        entity.set("classname", classname);
        if self
            .entity_class(classname)
            .is_some_and(|class| class.kind == ClassKind::Solid)
        {
            for (key, value) in self.class_defaults(classname) {
                entity.set(&key, value);
            }
        }
        let count = solids.len();
        entity.items.extend(solids.iter().cloned().map(EntityItem::Solid));
        let mut edits = removals(&solids, &emptied);
        edits.push(Edit::AddEntity { index: None, entity });
        self.apply_edit(&Edit::Batch(edits))?;
        self.select([Owner::Entity(id)], SelectMode::Replace);
        self.message = format!("Tied {count} brushes to a new {classname}");
        Ok(id)
    }

    /// Adds the other selected brushes, and the brushes of the other selected brush
    /// entities, to the brush entity `target`, as Hammer does when the selection already
    /// holds an entity. The emptied entities are removed. One undo step.
    pub fn tie_to_existing_entity(&mut self, target: u32) -> Result<(), String> {
        let classname = self
            .entity(target)
            .filter(|entity| entity.solids().next().is_some())
            .map(|entity| entity.classname().to_owned())
            .ok_or_else(|| format!("entity {target} is not a brush entity"))?;
        let (solids, emptied) = self.solids_to_tie(Some(target))?;
        let count = solids.len();
        let mut edits = removals(&solids, &emptied);
        edits.extend(solids.into_iter().map(|solid| Edit::AddSolid {
            entity: target,
            index: None,
            solid,
        }));
        self.apply_edit(&Edit::Batch(edits))?;
        self.select([Owner::Entity(target)], SelectMode::Replace);
        self.message = format!("Added {count} brushes to the {classname}");
        Ok(())
    }

    /// Hammer's Move to World: the brushes of every selected brush entity go back into the
    /// world and the entities are removed, keyvalues and all. The brushes end up selected.
    /// One undo step. Returns how many brushes moved.
    pub fn move_to_world(&mut self) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world = vmf.world().and_then(Entity::id).ok_or("the map has no world")?;
        let mut solids = Vec::new();
        let mut emptied = Vec::new();
        for owner in &self.selection {
            let Owner::Entity(id) = *owner else { continue };
            let entity = find_entity(vmf, id)?;
            if entity.solids().next().is_none() {
                continue;
            }
            solids.extend(entity.solids().cloned());
            emptied.push(id);
        }
        if solids.is_empty() {
            return Err("select a brush entity to move its brushes to the world".to_owned());
        }
        let moved: Vec<Owner> = solids.iter().filter_map(Solid::id).map(Owner::Solid).collect();
        let count = solids.len();
        let mut edits = removals(&solids, &emptied);
        edits.extend(solids.into_iter().map(|solid| Edit::AddSolid {
            entity: world,
            index: None,
            solid,
        }));
        self.apply_edit(&Edit::Batch(edits))?;
        self.select(moved, SelectMode::Replace);
        self.message = format!("Moved {count} brushes to the world");
        Ok(count)
    }

    /// The brushes Tie to Entity moves: every selected world brush and every brush of
    /// the selected brush entities other than `keep`, with the ids of those entities.
    fn solids_to_tie(&self, keep: Option<u32>) -> Result<(Vec<Solid>, Vec<u32>), String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world = vmf.world().ok_or("the map has no world")?;
        let mut solids = Vec::new();
        let mut emptied = Vec::new();
        for owner in &self.selection {
            match *owner {
                Owner::Solid(id) => {
                    if let Some(solid) = world.solids().find(|solid| solid.id() == Some(id)) {
                        solids.push(solid.clone());
                    }
                }
                Owner::Entity(id) if Some(id) != keep => {
                    let entity = find_entity(vmf, id)?;
                    if entity.solids().next().is_some() {
                        solids.extend(entity.solids().cloned());
                        emptied.push(id);
                    }
                }
                Owner::Entity(_) => {}
            }
        }
        if solids.is_empty() {
            return Err("select some brushes to tie to an entity".to_owned());
        }
        Ok((solids, emptied))
    }
}

/// Edits that take brushes out of the world and remove the entities they came from.
fn removals(solids: &[Solid], emptied: &[u32]) -> Vec<Edit> {
    let from_world = solids.iter().filter_map(Solid::id).map(|solid| Edit::RemoveSolid { solid });
    from_world
        .chain(emptied.iter().map(|entity| Edit::RemoveEntity { entity: *entity }))
        .collect()
}
