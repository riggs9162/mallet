//! Hammer's Group (Ctrl+G) and Ungroup (Ctrl+U): the `group` blocks of the world and the
//! `groupid` the members' `editor` blocks name.

use std::collections::BTreeSet;

use mallet_vmf::edit::Edit;
use mallet_vmf::kv::Block;
use mallet_vmf::{Entity, EntityItem, IdAllocator, Solid};

use super::clipboard::{group_block, set_group, solid_editor};
use super::{find_entity, Editor};
use crate::scene::Owner;

impl Editor {
    /// Hammer's Group: puts the selected brushes and entities in a new group, taking them
    /// out of any group they were in, so a click in Groups mode selects them together.
    /// Groups left empty are removed. One undo step.
    pub fn group_selection(&mut self) -> Result<(), String> {
        if self.selection.len() < 2 {
            return Err("select two or more objects to group".to_owned());
        }
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world = vmf.world().and_then(Entity::id).ok_or("the map has no world")?;
        let group = IdAllocator::for_map(vmf).object();
        let mut edits = vec![Edit::AddBlock {
            entity: world,
            index: None,
            block: group_block(group),
        }];
        let left = self.member_edits(Some(group), &mut edits)?;
        edits.extend(self.empty_group_removals(&left)?);
        self.apply_edit(&Edit::Batch(edits))?;
        self.message = format!("Grouped {} objects", self.selection.len());
        Ok(())
    }

    /// Hammer's Ungroup: every group the selection is in comes apart, its members staying
    /// where they are as separate objects. One undo step.
    pub fn ungroup_selection(&mut self) -> Result<(), String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let groups = self.selected_groups(vmf)?;
        if groups.is_empty() {
            return Err("the selection is not in a group".to_owned());
        }
        let mut edits = Vec::new();
        for entity in vmf.all_entities() {
            let world = entity.classname().eq_ignore_ascii_case("worldspawn");
            if world {
                let solids: Vec<Solid> = entity
                    .solids()
                    .filter(|solid| groups.contains(&group_of(solid.editor())))
                    .cloned()
                    .map(|mut solid| {
                        set_group(solid_editor(&mut solid), None);
                        solid
                    })
                    .collect();
                if !solids.is_empty() {
                    edits.push(Edit::ReplaceSolids(solids));
                }
            } else if groups.contains(&group_of(entity.editor())) {
                edits.extend(entity_group_edits(entity, None));
            }
        }
        edits.extend(self.group_block_removals(&groups)?);
        self.apply_edit(&Edit::Batch(edits))?;
        let noun = if groups.len() == 1 { "group" } else { "groups" };
        self.message = format!("Ungrouped {} {noun}", groups.len());
        Ok(())
    }

    /// Edits that put every selected object in `group`, or in none. Returns the groups the
    /// objects were in before.
    fn member_edits(&self, group: Option<u32>, edits: &mut Vec<Edit>) -> Result<BTreeSet<String>, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        vmf.world().ok_or("the map has no world")?;
        let mut before = BTreeSet::new();
        let mut solids = Vec::new();
        for owner in &self.selection {
            match *owner {
                Owner::Solid(id) => {
                    if let Some(solid) = vmf.world_solid(id) {
                        before.insert(group_of(solid.editor()));
                        let mut solid = solid.clone();
                        set_group(solid_editor(&mut solid), group);
                        solids.push(solid);
                    }
                }
                Owner::Entity(id) => {
                    let entity = find_entity(vmf, id)?;
                    before.insert(group_of(entity.editor()));
                    edits.extend(entity_group_edits(entity, group));
                }
            }
        }
        if !solids.is_empty() {
            edits.push(Edit::ReplaceSolids(solids));
        }
        before.remove("");
        Ok(before)
    }

    /// The groups the selected objects are in.
    fn selected_groups(&self, vmf: &mallet_vmf::Vmf) -> Result<BTreeSet<String>, String> {
        vmf.world().ok_or("the map has no world")?;
        let mut groups = BTreeSet::new();
        for owner in &self.selection {
            let group = match *owner {
                Owner::Solid(id) => vmf.world_solid(id)
                    .map(|solid| group_of(solid.editor())),
                Owner::Entity(id) => Some(group_of(find_entity(vmf, id)?.editor())),
            };
            groups.extend(group.filter(|group| !group.is_empty()));
        }
        Ok(groups)
    }

    /// Edits removing those of `groups` that no object other than the selection is in.
    fn empty_group_removals(&self, groups: &BTreeSet<String>) -> Result<Vec<Edit>, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let empty: BTreeSet<String> = groups
            .iter()
            .filter(|group| {
                !vmf.all_entities().any(|entity| {
                    let world = entity.classname().eq_ignore_ascii_case("worldspawn");
                    let entity_member = !world
                        && entity.id().is_some_and(|id| !self.selection.contains(&Owner::Entity(id)))
                        && group_of(entity.editor()) == **group;
                    let solid_member = world
                        && entity.solids().any(|solid| {
                            solid.id().is_some_and(|id| !self.selection.contains(&Owner::Solid(id)))
                                && group_of(solid.editor()) == **group
                        });
                    entity_member || solid_member
                })
            })
            .cloned()
            .collect();
        self.group_block_removals(&empty)
    }

    /// Edits removing the world's `group` blocks with these ids, last first so the
    /// positions stay right.
    fn group_block_removals(&self, groups: &BTreeSet<String>) -> Result<Vec<Edit>, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world = vmf.world().ok_or("the map has no world")?;
        let world_id = world.id().ok_or("the world has no id")?;
        let mut indices: Vec<usize> = world
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item {
                EntityItem::Other(block) if block.name.eq_ignore_ascii_case("group") => {
                    groups.contains(block.get("id")?.trim()).then_some(index)
                }
                _ => None,
            })
            .collect();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        Ok(indices
            .into_iter()
            .map(|index| Edit::RemoveBlock { entity: world_id, index })
            .collect())
    }
}

/// The group an `editor` block names, or an empty string for none.
fn group_of(editor: Option<&Block>) -> String {
    editor
        .and_then(|editor| editor.get("groupid"))
        .map(|group| group.trim().to_owned())
        .unwrap_or_default()
}

/// Edits that put an entity in `group`, or in none, by replacing its `editor` block.
fn entity_group_edits(entity: &Entity, group: Option<u32>) -> Vec<Edit> {
    let Some(id) = entity.id() else {
        return Vec::new();
    };
    let position = entity
        .items
        .iter()
        .position(|item| matches!(item, EntityItem::Other(block) if block.name.eq_ignore_ascii_case("editor")));
    let mut editor = position
        .and_then(|index| match &entity.items[index] {
            EntityItem::Other(block) => Some(block.clone()),
            _ => None,
        })
        .unwrap_or_else(|| Block::new("editor"));
    set_group(&mut editor, group);
    let mut edits = Vec::new();
    if let Some(index) = position {
        edits.push(Edit::RemoveBlock { entity: id, index });
    }
    edits.push(Edit::AddBlock {
        entity: id,
        index: position,
        block: editor,
    });
    edits
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;
    use crate::editor::SelectMode;

    #[test]
    fn grouping_and_ungrouping_round_trip() {
        let mut editor = Editor::default();
        editor.new_map();
        let a = editor.create_block(DVec3::ZERO, DVec3::splat(64.0)).unwrap();
        let b = editor.create_block(DVec3::splat(128.0), DVec3::splat(192.0)).unwrap();
        let light = editor.create_entity("light", DVec3::splat(32.0), &[]).unwrap();
        editor.select([Owner::Solid(a), Owner::Solid(b), Owner::Entity(light)], SelectMode::Replace);
        editor.group_selection().unwrap();
        let vmf = &editor.document.as_ref().unwrap().vmf;
        let world = vmf.world().unwrap();
        let groups = world
            .items
            .iter()
            .filter(|item| matches!(item, EntityItem::Other(block) if block.name == "group"))
            .count();
        assert_eq!(groups, 1);
        let group = group_of(world.solids().next().unwrap().editor());
        assert!(!group.is_empty());
        assert_eq!(group_of(find_entity(vmf, light).unwrap().editor()), group);
        editor.select([Owner::Solid(a)], SelectMode::Replace);
        editor.ungroup_selection().unwrap();
        let vmf = &editor.document.as_ref().unwrap().vmf;
        assert!(vmf.world().unwrap().solids().all(|solid| group_of(solid.editor()).is_empty()));
        assert!(group_of(find_entity(vmf, light).unwrap().editor()).is_empty());
        assert!(!vmf
            .world()
            .unwrap()
            .items
            .iter()
            .any(|item| matches!(item, EntityItem::Other(block) if block.name == "group")));
    }
}
