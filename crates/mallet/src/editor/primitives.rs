//! The Block tool's other shapes: Hammer's wedge, cylinder, spike, sphere and arch.

use glam::DVec3;
use mallet_vmf::edit::Edit;
use mallet_vmf::geometry::Bounds;
use mallet_vmf::primitive::{arch_solids, primitive_solid, Arch, Shape};
use mallet_vmf::{Entity, IdAllocator};

use super::clipboard::{group_block, set_group, solid_editor};
use super::{Editor, SelectMode};
use crate::scene::Owner;

/// The number of faces round primitives start with, as in Hammer's Objects bar.
pub const DEFAULT_FACES: u32 = 8;

impl Editor {
    /// Creates the Block tool's current shape in the box between two corners, with the
    /// current material, and selects it. Arches need their dialog, so they go through
    /// [`Editor::create_arch`]. Returns the new brush's id.
    pub fn create_primitive(&mut self, shape: Shape, min: DVec3, max: DVec3) -> Result<u32, String> {
        if shape == Shape::Block {
            return self.create_block(min, max);
        }
        if shape == Shape::Arch {
            return Err("arches are set up in the Arch Properties dialog".to_owned());
        }
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world = vmf.world().and_then(Entity::id).ok_or("the map has no world")?;
        let mut ids = IdAllocator::for_map(vmf);
        let solid = primitive_solid(shape, min, max, self.block_faces, &self.material, &mut ids)
            .ok_or_else(|| format!("the box is too small for a {}", shape.label()))?;
        let id = solid.id().ok_or("the new brush has no id")?;
        self.apply_edit(&Edit::AddSolid {
            entity: world,
            index: None,
            solid,
        })?;
        self.finish_new_brushes(min, max, vec![Owner::Solid(id)]);
        self.message = format!("Created {} {id}", shape.label());
        Ok(id)
    }

    /// Creates an arch in the box between two corners with the current material, one
    /// brush per side, grouped as Hammer groups them, and selects it. One undo step.
    /// Returns the new brushes' ids.
    pub fn create_arch(&mut self, min: DVec3, max: DVec3, arch: &Arch) -> Result<Vec<u32>, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world = vmf.world().and_then(Entity::id).ok_or("the map has no world")?;
        let mut ids = IdAllocator::for_map(vmf);
        let mut pieces = arch_solids(min, max, arch, &self.material, &mut ids);
        if pieces.is_empty() {
            return Err("the box is too small for an arch".to_owned());
        }
        let group = ids.object();
        let mut edits = vec![Edit::AddBlock {
            entity: world,
            index: None,
            block: group_block(group),
        }];
        let mut made = Vec::new();
        for piece in &mut pieces {
            set_group(solid_editor(piece), Some(group));
        }
        for piece in pieces {
            made.extend(piece.id());
            edits.push(Edit::AddSolid {
                entity: world,
                index: None,
                solid: piece,
            });
        }
        self.apply_edit(&Edit::Batch(edits))?;
        self.finish_new_brushes(min, max, made.iter().copied().map(Owner::Solid).collect());
        self.message = format!("Created an arch of {} brushes", made.len());
        Ok(made)
    }

    /// After a shape is made: new blocks reuse its depth, the pending box goes, and the
    /// new brushes are selected.
    fn finish_new_brushes(&mut self, min: DVec3, max: DVec3, made: Vec<Owner>) {
        self.last_block = Bounds {
            min: min.min(max),
            max: min.max(max),
        };
        self.overlay.block = None;
        self.select(made, SelectMode::Replace);
    }
}
