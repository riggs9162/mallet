//! Hammer's Carve (Ctrl+Shift+C) from the editor: the selected brushes cut their shape
//! out of every other brush they overlap.

use std::collections::{BTreeSet, HashMap};

use mallet_vmf::carve::carve_solid;
use mallet_vmf::clip::{remap_side_list, renumber_sides, ClipError};
use mallet_vmf::edit::Edit;
use mallet_vmf::{IdAllocator, Solid, Vmf};

use super::{find_entity, Editor};
use crate::scene::Owner;

impl Editor {
    /// Hammer's Carve: every selected brush, and every brush of a selected entity, cuts
    /// its shape out of the other brushes it overlaps, in the world and in brush entities.
    /// The pieces stay where their brush was, each new face takes the texture of the
    /// carving face it lies on, and overlays on cut faces keep them. The carving brushes
    /// stay selected. One undo step. Returns how many brushes were carved.
    pub fn carve(&mut self) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let carvers = selected_solids(vmf, &self.selection)?;
        if carvers.is_empty() {
            return Err("select the brushes to carve with".to_owned());
        }
        let carver_ids: BTreeSet<u32> = carvers.iter().filter_map(Solid::id).collect();
        let mut ids = IdAllocator::for_map(vmf);
        let mut edits = Vec::new();
        let mut faces: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut carved = 0;
        let mut displacements = 0;
        for entity in vmf.all_entities() {
            let Some(owner) = entity.id() else { continue };
            for target in entity.solids() {
                let Some(target_id) = target.id().filter(|id| !carver_ids.contains(id)) else {
                    continue;
                };
                let mut pieces = vec![target.clone()];
                let mut changed = false;
                for carver in &carvers {
                    let mut next = Vec::with_capacity(pieces.len());
                    for piece in pieces {
                        match carve_solid(&piece, carver, &mut ids) {
                            Ok(Some(outside)) => {
                                changed = true;
                                next.extend(outside);
                            }
                            Ok(None) => next.push(piece),
                            Err(ClipError::Displacement) => {
                                displacements += 1;
                                next.push(piece);
                            }
                            Err(_) => next.push(piece),
                        }
                    }
                    pieces = next;
                }
                if !changed {
                    continue;
                }
                carved += 1;
                let originals: BTreeSet<u32> = target.sides().map(|side| side.id).collect();
                for side in &originals {
                    faces.entry(*side).or_default();
                }
                edits.push(Edit::RemoveSolid { solid: target_id });
                for mut piece in pieces {
                    for (old, new) in renumber_sides(&mut piece, &mut ids) {
                        if originals.contains(&old) {
                            faces.entry(old).or_default().push(new);
                        }
                    }
                    edits.push(Edit::AddSolid {
                        entity: owner,
                        index: None,
                        solid: piece,
                    });
                }
            }
        }
        if carved == 0 {
            return Err(match displacements {
                0 => "the selection does not overlap any other brush".to_owned(),
                _ => "displacements cannot be carved".to_owned(),
            });
        }
        edits.extend(side_list_edits(vmf, &faces));
        self.apply_edit(&Edit::Batch(edits))?;
        let noun = if carved == 1 { "brush" } else { "brushes" };
        self.message = format!("Carved {carved} {noun}");
        Ok(carved)
    }
}

/// The selected world brushes and the brushes of selected entities.
fn selected_solids(vmf: &Vmf, selection: &BTreeSet<Owner>) -> Result<Vec<Solid>, String> {
    let world = vmf.world().ok_or("the map has no world")?;
    let mut solids = Vec::new();
    for owner in selection {
        match *owner {
            Owner::Solid(id) => solids.extend(world.solids().find(|solid| solid.id() == Some(id)).cloned()),
            Owner::Entity(id) => solids.extend(find_entity(vmf, id)?.solids().cloned()),
        }
    }
    Ok(solids)
}

/// Key edits that point every entity listing carved faces in `sides`, such as overlays,
/// at the faces those became.
fn side_list_edits(vmf: &Vmf, faces: &HashMap<u32, Vec<u32>>) -> Vec<Edit> {
    vmf.entities()
        .filter_map(|entity| {
            let id = entity.id()?;
            let value = remap_side_list(entity.get("sides")?, faces)?;
            Some(Edit::SetKey {
                entity: id,
                key: "sides".to_owned(),
                value,
            })
        })
        .collect()
}
