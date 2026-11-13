//! Hammer's Make Hollow from the editor: the selected brushes become grouped walls.

use mallet_vmf::edit::Edit;
use mallet_vmf::hollow::hollow_solid;
use mallet_vmf::{Entity, IdAllocator, Solid};

use super::clipboard::{group_block, set_group, solid_editor};
use super::{find_entity, Editor, SelectMode};
use crate::scene::Owner;

/// The wall width Hammer's Make Hollow dialog starts with.
pub const DEFAULT_HOLLOW_WIDTH: f64 = 32.0;

impl Editor {
    /// Hammer's Make Hollow: every selected brush, including the brushes of selected brush
    /// entities, becomes walls `thickness` thick around the space it filled, or outside it
    /// when `thickness` is negative. The walls of each world brush are grouped, as Hammer
    /// groups them, and the walls end up selected. One undo step. Returns how many walls
    /// were made.
    pub fn make_hollow(&mut self, thickness: f64) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world = vmf.world().ok_or("the map has no world")?;
        let world_id = world.id().ok_or("the world has no id")?;
        let mut targets: Vec<(u32, Solid)> = Vec::new();
        for owner in &self.selection {
            match *owner {
                Owner::Solid(id) => {
                    if let Some(solid) = vmf.world_solid(id) {
                        targets.push((world_id, solid.clone()));
                    }
                }
                Owner::Entity(id) => {
                    let entity: &Entity = find_entity(vmf, id)?;
                    targets.extend(entity.solids().map(|solid| (id, solid.clone())));
                }
            }
        }
        if targets.is_empty() {
            return Err("select the brushes to hollow".to_owned());
        }
        let mut ids = IdAllocator::for_map(vmf);
        let mut edits = Vec::new();
        let mut made = Vec::new();
        let brushes = targets.len();
        for (owner, solid) in targets {
            let id = solid.id().ok_or("a selected brush has no id")?;
            let walls = hollow_solid(&solid, thickness, &mut ids)
                .map_err(|error| format!("Could not hollow brush {id}: {error}"))?;
            let group = (owner == world_id).then(|| ids.object());
            edits.push(Edit::RemoveSolid { solid: id });
            if let Some(group) = group {
                edits.push(Edit::AddBlock {
                    entity: world_id,
                    index: None,
                    block: group_block(group),
                });
            }
            for mut wall in walls {
                set_group(solid_editor(&mut wall), group);
                made.push(match (group, wall.id()) {
                    (Some(_), Some(wall)) => Owner::Solid(wall),
                    _ => Owner::Entity(owner),
                });
                edits.push(Edit::AddSolid {
                    entity: owner,
                    index: None,
                    solid: wall,
                });
            }
        }
        let count = made.len();
        self.apply_edit(&Edit::Batch(edits))?;
        self.select(made, SelectMode::Replace);
        let noun = if brushes == 1 { "brush" } else { "brushes" };
        self.message = format!("Hollowed {brushes} {noun} into {count} walls {} units thick", thickness.abs());
        Ok(count)
    }
}
