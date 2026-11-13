//! Finds entities and brushes by id without reading the whole map each time.
//!
//! A map's items are public and change freely, so the remembered positions are only a
//! hint. Every lookup checks that the item found still has the id, looks next to it when
//! the map has shifted a little, and reads the whole map again when that fails or the
//! number of the map's items is no longer what it was. A lookup therefore never returns
//! a wrong item, however the map was changed in between.
//!
//! An id the positions do not hold is taken to be absent from the map. Edits tell the
//! positions what they add, so that holds for every map changed through [`Edit`]s.
//! Code that puts a brush into an entity's items by hand calls
//! [`Vmf::forget_positions`] afterwards.
//!
//! [`Edit`]: crate::edit::Edit

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::model::{Entity, EntityItem, Solid, TopLevel, Vmf};

/// Where each entity and brush of a map was when it was last read, by id.
#[derive(Default)]
pub(crate) struct IdIndex(Mutex<Option<Positions>>);

struct Positions {
    /// The position of each world or entity among the map's items.
    entities: HashMap<u32, usize>,
    /// The position of each brush: its owner among the map's items, then the brush
    /// among the owner's items.
    solids: HashMap<u32, (usize, usize)>,
    /// How many items the map had when the positions were last brought up to date.
    count: usize,
}

/// How many places to either side of where an item was it is looked for before the
/// whole map is read again.
const NEAR: usize = 64;

impl IdIndex {
    fn lock(&self) -> MutexGuard<'_, Option<Positions>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drops the remembered positions, so the next lookup reads the map again.
    pub(crate) fn forget(&self) {
        *self.lock() = None;
    }

    /// Notes an entity an edit has just put at `at` among the map's items, with its
    /// brushes.
    pub(crate) fn note_entity(&self, at: usize, entity: &Entity) {
        let mut guard = self.lock();
        let Some(positions) = guard.as_mut() else {
            return;
        };
        positions.count += 1;
        if let Some(id) = entity.id() {
            positions.entities.entry(id).or_insert(at);
        }
        for (inside, item) in entity.items.iter().enumerate() {
            if let EntityItem::Solid(solid) = item {
                if let Some(id) = solid.id() {
                    positions.solids.entry(id).or_insert((at, inside));
                }
            }
        }
    }

    /// Notes that an edit has just taken an item out of the map's items.
    pub(crate) fn note_removed(&self) {
        if let Some(positions) = self.lock().as_mut() {
            positions.count = positions.count.saturating_sub(1);
        }
    }

    /// Notes a brush an edit has just put at `inside` among the items of the entity at
    /// `owner`.
    pub(crate) fn note_solid(&self, id: u32, owner: usize, inside: usize) {
        if let Some(positions) = self.lock().as_mut() {
            positions.solids.entry(id).or_insert((owner, inside));
        }
    }
}

impl Clone for IdIndex {
    /// A copy of a map starts without remembered positions and reads them when needed.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for IdIndex {
    /// Remembered positions are no part of a map, so they never make two maps differ.
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl fmt::Debug for IdIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdIndex")
    }
}

impl Positions {
    /// Reads the position of everything with an id. Of items that share an id the first
    /// in file order counts, as it does for a search from the top of the map.
    fn read(items: &[TopLevel]) -> Self {
        let mut positions = Self {
            entities: HashMap::new(),
            solids: HashMap::new(),
            count: items.len(),
        };
        for (at, item) in items.iter().enumerate() {
            let (TopLevel::World(entity) | TopLevel::Entity(entity)) = item else {
                continue;
            };
            if let Some(id) = entity.id() {
                positions.entities.entry(id).or_insert(at);
            }
            for (inside, item) in entity.items.iter().enumerate() {
                if let EntityItem::Solid(solid) = item {
                    if let Some(id) = solid.id() {
                        positions.solids.entry(id).or_insert((at, inside));
                    }
                }
            }
        }
        positions
    }

    /// The positions of a map, read anew when there are none yet or the map has gained
    /// or lost items that no edit told them of.
    fn of<'a>(slot: &'a mut Option<Self>, items: &[TopLevel]) -> &'a mut Self {
        if slot.as_ref().is_some_and(|positions| positions.count != items.len()) {
            *slot = None;
        }
        slot.get_or_insert_with(|| Self::read(items))
    }
}

/// The position nearest to `from` among `0..len`, and no further than [`NEAR`] from
/// it, that `matches`, trying `from` itself first: where an item ends up after a few
/// others were added or removed around it.
fn nearest(from: usize, len: usize, matches: impl Fn(usize) -> bool) -> Option<usize> {
    let from = from.min(len.checked_sub(1)?);
    if matches(from) {
        return Some(from);
    }
    (1..=NEAR).find_map(|step| {
        let below = from.checked_sub(step);
        let above = Some(from + step).filter(|at| *at < len);
        below.into_iter().chain(above).find(|at| matches(*at))
    })
}

fn entity_at(items: &[TopLevel], at: usize) -> Option<&Entity> {
    match items.get(at)? {
        TopLevel::World(entity) | TopLevel::Entity(entity) => Some(entity),
        _ => None,
    }
}

fn solid_at(entity: &Entity, at: usize) -> Option<&Solid> {
    match entity.items.get(at)? {
        EntityItem::Solid(solid) => Some(solid),
        _ => None,
    }
}

impl Vmf {
    /// Makes the next lookup by id read the whole map again. Only needed after putting
    /// a brush into an entity's items by hand; [`Edit`](crate::edit::Edit)s and changes
    /// to the map's own items are followed without it.
    pub fn forget_positions(&self) {
        self.index.forget();
    }

    /// The position among the map's items of the world or entity with this id.
    pub fn entity_position(&self, id: u32) -> Option<usize> {
        let has_id = |at: usize| entity_at(&self.items, at).is_some_and(|entity| entity.id() == Some(id));
        let mut guard = self.index.lock();
        let positions = Positions::of(&mut guard, &self.items);
        let remembered = *positions.entities.get(&id)?;
        if let Some(found) = nearest(remembered, self.items.len(), has_id) {
            if found != remembered {
                positions.entities.insert(id, found);
            }
            return Some(found);
        }
        *positions = Positions::read(&self.items);
        positions.entities.get(&id).copied()
    }

    /// The world or entity with this id.
    pub fn entity_by_id(&self, id: u32) -> Option<&Entity> {
        entity_at(&self.items, self.entity_position(id)?)
    }

    /// The world or entity with this id, to change it.
    pub fn entity_by_id_mut(&mut self, id: u32) -> Option<&mut Entity> {
        let at = self.entity_position(id)?;
        match self.items.get_mut(at)? {
            TopLevel::World(entity) | TopLevel::Entity(entity) => Some(entity),
            _ => None,
        }
    }

    /// The position of the brush with this id: its owner among the map's items, then the
    /// brush among the owner's items.
    pub fn solid_position(&self, id: u32) -> Option<(usize, usize)> {
        let has_id = |entity: &Entity, at: usize| solid_at(entity, at).is_some_and(|solid| solid.id() == Some(id));
        let mut guard = self.index.lock();
        let positions = Positions::of(&mut guard, &self.items);
        let (owner, remembered) = *positions.solids.get(&id)?;
        let found = entity_at(&self.items, owner)
            .and_then(|entity| nearest(remembered, entity.items.len(), |at| has_id(entity, at)));
        if let Some(found) = found {
            if found != remembered {
                positions.solids.insert(id, (owner, found));
            }
            return Some((owner, found));
        }
        *positions = Positions::read(&self.items);
        positions.solids.get(&id).copied()
    }

    /// The brush with this id, of the world or of an entity.
    pub fn solid_by_id(&self, id: u32) -> Option<&Solid> {
        self.solid_with_owner(id).map(|(_, solid)| solid)
    }

    /// The world brush with this id; `None` for a brush of an entity.
    pub fn world_solid(&self, id: u32) -> Option<&Solid> {
        let (owner, inside) = self.solid_position(id)?;
        match self.items.get(owner)? {
            TopLevel::World(world) => solid_at(world, inside),
            _ => None,
        }
    }

    /// The brush with this id, after the world or entity it belongs to.
    pub fn solid_with_owner(&self, id: u32) -> Option<(&Entity, &Solid)> {
        let (owner, inside) = self.solid_position(id)?;
        let entity = entity_at(&self.items, owner)?;
        Some((entity, solid_at(entity, inside)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::Edit;

    const MAP: &str = r#"
world
{
	"id" "1"
	"classname" "worldspawn"
	solid
	{
		"id" "10"
	}
	solid
	{
		"id" "11"
	}
}
entity
{
	"id" "2"
	"classname" "light"
}
entity
{
	"id" "3"
	"classname" "func_detail"
	solid
	{
		"id" "12"
	}
}
"#;

    fn classname(vmf: &Vmf, id: u32) -> Option<&str> {
        vmf.entity_by_id(id).map(Entity::classname)
    }

    #[test]
    fn finds_entities_and_brushes_by_id() {
        let vmf = Vmf::parse(MAP).unwrap();
        assert_eq!(classname(&vmf, 1), Some("worldspawn"));
        assert_eq!(classname(&vmf, 3), Some("func_detail"));
        assert_eq!(classname(&vmf, 4), None);
        assert_eq!(vmf.solid_by_id(11).and_then(Solid::id), Some(11));
        assert_eq!(vmf.solid_by_id(12).and_then(Solid::id), Some(12));
        assert!(vmf.solid_by_id(13).is_none());
    }

    #[test]
    fn follows_items_that_were_removed_and_put_back() {
        let mut vmf = Vmf::parse(MAP).unwrap();
        assert_eq!(classname(&vmf, 3), Some("func_detail"));
        assert!(vmf.solid_by_id(11).is_some());

        let restore_light = Edit::RemoveEntity { entity: 2 }.apply(&mut vmf).unwrap();
        let restore_brush = Edit::RemoveSolid { solid: 10 }.apply(&mut vmf).unwrap();
        assert_eq!(classname(&vmf, 2), None);
        assert_eq!(classname(&vmf, 3), Some("func_detail"));
        assert!(vmf.solid_by_id(10).is_none());
        assert_eq!(vmf.solid_by_id(11).and_then(Solid::id), Some(11));
        assert_eq!(vmf.solid_by_id(12).and_then(Solid::id), Some(12));

        restore_brush.apply(&mut vmf).unwrap();
        restore_light.apply(&mut vmf).unwrap();
        assert_eq!(classname(&vmf, 2), Some("light"));
        assert_eq!(classname(&vmf, 3), Some("func_detail"));
        assert_eq!(vmf.solid_by_id(10).and_then(Solid::id), Some(10));
        assert_eq!(vmf.solid_by_id(12).and_then(Solid::id), Some(12));
    }

    #[test]
    fn follows_items_changed_directly() {
        let mut vmf = Vmf::parse(MAP).unwrap();
        assert_eq!(classname(&vmf, 2), Some("light"));
        vmf.items.reverse();
        assert_eq!(classname(&vmf, 2), Some("light"));
        assert_eq!(classname(&vmf, 1), Some("worldspawn"));
        assert_eq!(vmf.solid_by_id(12).and_then(Solid::id), Some(12));
        vmf.items.clear();
        assert_eq!(classname(&vmf, 2), None);
        assert!(vmf.solid_by_id(12).is_none());
    }

    #[test]
    fn follows_many_removals_and_additions() {
        let mut text = String::from("world\n{\n\t\"id\" \"1\"\n\t\"classname\" \"worldspawn\"\n");
        for id in 0..300 {
            text.push_str(&format!("\tsolid\n\t{{\n\t\t\"id\" \"{}\"\n\t}}\n", 1000 + id));
        }
        text.push_str("}\n");
        for id in 0..300 {
            text.push_str(&format!(
                "entity\n{{\n\t\"id\" \"{}\"\n\t\"classname\" \"light\"\n}}\n",
                2000 + id
            ));
        }
        let mut vmf = Vmf::parse(&text).unwrap();
        assert!(vmf.entity_by_id(2299).is_some() && vmf.solid_by_id(1299).is_some());

        let removals: Vec<Edit> = (0..200)
            .flat_map(|id| {
                [
                    Edit::RemoveEntity { entity: 2000 + id },
                    Edit::RemoveSolid { solid: 1000 + id },
                ]
            })
            .collect();
        let restore = Edit::Batch(removals).apply(&mut vmf).unwrap();
        for id in 0..300 {
            assert_eq!(vmf.entity_by_id(2000 + id).is_some(), id >= 200, "entity {id}");
            assert_eq!(vmf.solid_by_id(1000 + id).is_some(), id >= 200, "brush {id}");
        }
        restore.apply(&mut vmf).unwrap();
        for id in 0..300 {
            assert_eq!(vmf.entity_by_id(2000 + id).and_then(Entity::id), Some(2000 + id));
            assert_eq!(vmf.solid_by_id(1000 + id).and_then(Solid::id), Some(1000 + id));
        }
    }

    #[test]
    fn finds_a_brush_added_by_hand_once_told() {
        let mut vmf = Vmf::parse(MAP).unwrap();
        assert!(vmf.solid_by_id(99).is_none());
        let brush = Vmf::parse("world\n{\n\tsolid\n\t{\n\t\t\"id\" \"99\"\n\t}\n}\n").unwrap();
        let TopLevel::World(source) = &brush.items[0] else {
            panic!("the map has a world");
        };
        let TopLevel::World(world) = &mut vmf.items[0] else {
            panic!("the map has a world");
        };
        world.items.extend(source.items.iter().cloned());
        vmf.forget_positions();
        assert_eq!(vmf.solid_by_id(99).and_then(Solid::id), Some(99));
    }
}
