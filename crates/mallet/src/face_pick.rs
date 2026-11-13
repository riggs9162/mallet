//! Picking brush faces for a keyvalue that lists them, such as an overlay's `sides`:
//! Hammer's Pick button next to a side list in Object Properties. While picking, a click
//! on a brush face in the 3D view makes it the only face listed, and Ctrl+click adds it
//! or takes it off, for every entity the window edits. The listed faces are marked in
//! the views the way the texture tool marks its selection.

use std::collections::BTreeSet;

use glam::DVec3;
use mallet_vmf::edit::Edit;

use crate::editor::Editor;

/// The side list key that clicks in the 3D view pick faces for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacePick {
    /// The tab the entities are in, by its lasting id.
    pub tab: u64,
    /// The entities whose key is picked, as the Object Properties window edits them.
    pub ids: Vec<u32>,
    /// The key, such as `sides` or `sides2`.
    pub key: String,
}

/// How a picked face changes a side list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickMode {
    /// The face becomes the only one listed.
    Replace,
    /// The face is listed when it is not yet.
    Add,
    /// The face is taken off the list.
    Remove,
    /// The face is taken off every list that has it when all of them do, and added to
    /// the others otherwise.
    Toggle,
}

/// The side ids a side list key's value names, in order and without repeats.
pub fn side_list(value: &str) -> Vec<u32> {
    let mut sides = Vec::new();
    for side in value.split_whitespace().filter_map(|word| word.parse::<u32>().ok()) {
        if !sides.contains(&side) {
            sides.push(side);
        }
    }
    sides
}

fn join_sides(sides: &[u32]) -> String {
    sides.iter().map(u32::to_string).collect::<Vec<_>>().join(" ")
}

impl Editor {
    /// Starts picking faces for `key` of the entities `ids` with clicks in the 3D view,
    /// replacing any pick already running.
    pub fn start_face_pick(&mut self, ids: Vec<u32>, key: &str) {
        self.position_pick = None;
        self.face_pick = Some(FacePick {
            tab: self.active_tab_id(),
            ids,
            key: key.to_owned(),
        });
        self.message = format!(
            "Picking faces for {key}: click a face in the 3D view to list only it, Ctrl+click to add or take \
             off a face, Escape to stop"
        );
        self.refresh_face_marks();
    }

    /// Stops picking faces. Returns whether a pick was running.
    pub fn stop_face_pick(&mut self) -> bool {
        let running = self.face_pick.take().is_some();
        if running {
            self.message = "Stopped picking faces".to_owned();
            self.refresh_face_marks();
        }
        running
    }

    /// The face pick that clicks in the window's 3D view go to: the running one, while
    /// its tab is the one shown and no agent tool call is running in its own view.
    pub fn active_face_pick(&self) -> Option<&FacePick> {
        self.face_pick
            .as_ref()
            .filter(|pick| !self.in_agent_view() && pick.tab == self.active_tab_id())
    }

    /// Every face the picked key lists on any of its entities, while a pick is active.
    pub fn picked_sides(&self) -> Option<BTreeSet<u32>> {
        let pick = self.active_face_pick()?;
        Some(
            pick.ids
                .iter()
                .filter_map(|id| self.entity(*id)?.get(&pick.key))
                .flat_map(side_list)
                .collect(),
        )
    }

    /// Marks the faces the views highlight: those of the active face pick, or else the
    /// texture tool's selection.
    pub fn refresh_face_marks(&mut self) {
        match self.picked_sides() {
            Some(sides) => self.scene.mark_faces(&sides),
            None => self.scene.mark_faces(&self.face_selection),
        }
    }

    /// A click along a ray while picking faces: the brush face it meets, seeing through
    /// entities, props, overlays and decals, goes on the picked key. Ctrl adds the face
    /// or takes it off; a plain click lists only it.
    pub fn face_pick_click(&mut self, origin: DVec3, direction: DVec3, ctrl: bool) {
        let Some(pick) = self.active_face_pick().cloned() else {
            return;
        };
        let Some((hit, _)) = self.scene.surface_face(origin, direction) else {
            self.message = "Click on a brush face to pick it".to_owned();
            return;
        };
        let mode = if ctrl { PickMode::Toggle } else { PickMode::Replace };
        if let Err(error) = self.pick_side(&pick.ids, &pick.key, hit.side, mode) {
            self.message = error;
        }
    }

    /// Changes the side list `key` of the entities `ids` with one brush face, as one
    /// undoable step. Returns the faces each entity lists afterwards, by entity.
    pub fn pick_side(
        &mut self,
        ids: &[u32],
        key: &str,
        side: u32,
        mode: PickMode,
    ) -> Result<Vec<(u32, Vec<u32>)>, String> {
        if ids.is_empty() {
            return Err("pick faces for at least one entity".to_owned());
        }
        if !self
            .scene
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.faces)
            .any(|face| face.side == Some(side))
        {
            return Err(format!("no brush face has side id {side}"));
        }
        let mut lists = Vec::with_capacity(ids.len());
        for id in ids {
            let entity = self.entity(*id).ok_or_else(|| format!("no entity has id {id}"))?;
            lists.push((*id, side_list(entity.get(key).unwrap_or_default())));
        }
        let listed_everywhere = lists.iter().all(|(_, sides)| sides.contains(&side));
        let remove = match mode {
            PickMode::Remove => true,
            PickMode::Toggle => listed_everywhere,
            PickMode::Replace | PickMode::Add => false,
        };
        let mut edits = Vec::new();
        for (id, sides) in &mut lists {
            let before = sides.clone();
            match mode {
                PickMode::Replace => *sides = vec![side],
                _ if remove => sides.retain(|listed| *listed != side),
                _ if !sides.contains(&side) => sides.push(side),
                _ => {}
            }
            if *sides != before {
                edits.push(Edit::SetKey {
                    entity: *id,
                    key: key.to_owned(),
                    value: join_sides(sides),
                });
            }
        }
        match edits.len() {
            0 => {}
            1 => self.apply_edit(&edits.remove(0))?,
            _ => self.apply_edit(&Edit::Batch(edits))?,
        }
        let count = lists.iter().map(|(_, sides)| sides.len()).max().unwrap_or(0);
        let faces = if count == 1 { "face" } else { "faces" };
        self.message = match mode {
            PickMode::Replace => format!("{key} lists face {side}"),
            _ if remove => format!("Took face {side} off {key}; {count} {faces} listed"),
            _ => format!("Added face {side} to {key}; {count} {faces} listed"),
        };
        Ok(lists)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_lists_skip_repeats_and_junk() {
        assert_eq!(side_list(" 12 7 x 12  30 "), vec![12, 7, 30]);
        assert!(side_list("").is_empty());
        assert_eq!(join_sides(&[4, 9]), "4 9");
    }
}
