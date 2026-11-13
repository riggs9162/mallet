//! Picking a sound position for a soundscape entity: the crosshair button next to
//! `position0` to `position7` in Object Properties. While picking, a click on a surface
//! in the 3D view puts an `info_target` there under a name of its own and writes that
//! name into the key, for every entity the window edits. A key that already names a
//! single `info_target` moves that one instead, so picking again leaves nothing behind.

use glam::DVec3;
use mallet_vmf::edit::Edit;
use mallet_vmf::{format_vec3, Entity, IdAllocator};

use crate::editor::{class_plays_soundscape, Editor};

/// The class of the entities that mark sound positions.
const POSITION_CLASS: &str = "info_target";

/// The position key that the next click in the 3D view places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionPick {
    /// The tab the entities are in, by its lasting id.
    pub tab: u64,
    /// The soundscape entities whose key is picked, as the Object Properties window
    /// edits them.
    pub ids: Vec<u32>,
    /// The key, such as `position0`.
    pub key: String,
}

/// What placing a sound position did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedPosition {
    /// The `info_target` at the position.
    pub entity: u32,
    /// Its targetname, which the key now holds.
    pub name: String,
    /// Whether it was made now, and not moved from somewhere else.
    pub created: bool,
}

/// Whether `key` of an entity of class `classname` names a sound position: one of
/// `position0` to `position7` on an entity that plays a soundscape.
pub fn is_position_key(classname: &str, key: &str) -> bool {
    let number = key
        .get(..8)
        .filter(|start| start.eq_ignore_ascii_case("position"))
        .and_then(|_| key.get(8..));
    class_plays_soundscape(classname) && matches!(number, Some("0" | "1" | "2" | "3" | "4" | "5" | "6" | "7"))
}

impl Editor {
    /// Starts picking where `key` of the entities `ids` plays its sounds with a click in
    /// the 3D view, replacing any pick already running.
    pub fn start_position_pick(&mut self, ids: Vec<u32>, key: &str) {
        self.stop_face_pick();
        self.position_pick = Some(PositionPick {
            tab: self.active_tab_id(),
            ids,
            key: key.to_owned(),
        });
        self.message = format!("Picking {key}: click a surface in the 3D view to put it there, Escape to stop");
    }

    /// Stops picking a sound position. Returns whether a pick was running.
    pub fn stop_position_pick(&mut self) -> bool {
        let running = self.position_pick.take().is_some();
        if running {
            self.message = "Stopped picking a sound position".to_owned();
        }
        running
    }

    /// The position pick that clicks in the window's 3D view go to: the running one,
    /// while its tab is the one shown and no agent tool call is running in its own view.
    pub fn active_position_pick(&self) -> Option<&PositionPick> {
        self.position_pick
            .as_ref()
            .filter(|pick| !self.in_agent_view() && pick.tab == self.active_tab_id())
    }

    /// A click along a ray while picking a sound position: the point of the surface it
    /// meets, on whole units, becomes the position, and the pick ends.
    pub fn position_pick_click(&mut self, origin: DVec3, direction: DVec3) {
        let Some(pick) = self.active_position_pick().cloned() else {
            return;
        };
        let Some(point) = self.scene.surface_hit(origin, direction) else {
            self.message = format!("Click on a surface to put {} there", pick.key);
            return;
        };
        self.position_pick = None;
        if let Err(error) = self.place_position(&pick.ids, &pick.key, point.round() + DVec3::ZERO) {
            self.message = error;
        }
    }

    /// Makes `point` the sound position `key` of the soundscape entities `ids`, as one
    /// undoable step. When the key names the same single `info_target` on all of them,
    /// that entity moves to the point. Otherwise a new `info_target` is made there, named
    /// after the first entity and the key, and the key is set to its name. The selection
    /// stays as it is.
    pub fn place_position(&mut self, ids: &[u32], key: &str, point: DVec3) -> Result<PlacedPosition, String> {
        if ids.is_empty() {
            return Err("pick a position for at least one entity".to_owned());
        }
        for id in ids {
            let entity = self.entity(*id).ok_or_else(|| format!("no entity has id {id}"))?;
            if !is_position_key(entity.classname(), key) {
                return Err(format!("{key} of {} {id} is not a sound position", entity.classname()));
            }
        }
        let origin = format_vec3(point);
        if let Some((entity, name)) = self.named_position(ids, key) {
            self.apply_edit(&Edit::SetKey {
                entity,
                key: "origin".to_owned(),
                value: origin.clone(),
            })?;
            self.message = format!("Moved {name}, the {key} of the soundscape, to {origin}");
            return Ok(PlacedPosition {
                entity,
                name,
                created: false,
            });
        }
        let name = self.position_name(ids[0], key);
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let id = IdAllocator::for_map(vmf).object();
        let mut target = Entity::default();
        target.set("id", id.to_string());
        target.set("classname", POSITION_CLASS);
        for (default_key, value) in self.class_defaults(POSITION_CLASS) {
            target.set(&default_key, value);
        }
        target.set("targetname", name.clone());
        target.set("origin", origin.clone());
        let mut edits = vec![Edit::AddEntity {
            index: None,
            entity: target,
        }];
        edits.extend(ids.iter().map(|id| Edit::SetKey {
            entity: *id,
            key: key.to_owned(),
            value: name.clone(),
        }));
        self.apply_edit(&Edit::Batch(edits))?;
        self.message = format!("Made {POSITION_CLASS} {name} at {origin} and set {key} to it");
        Ok(PlacedPosition {
            entity: id,
            name,
            created: true,
        })
    }

    /// The single `info_target` that `key` names on every one of the entities `ids`, with
    /// its targetname as the map spells it.
    fn named_position(&self, ids: &[u32], key: &str) -> Option<(u32, String)> {
        let wanted = self.entity(ids[0])?.get(key)?.trim();
        if wanted.is_empty() {
            return None;
        }
        let same = ids.iter().all(|id| {
            self.entity(*id)
                .and_then(|entity| entity.get(key))
                .is_some_and(|value| value.trim().eq_ignore_ascii_case(wanted))
        });
        if !same {
            return None;
        }
        let mut named = self.document.as_ref()?.vmf.all_entities().filter(|entity| {
            entity
                .get("targetname")
                .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
        });
        let only = named.next()?;
        if named.next().is_some() || !only.classname().eq_ignore_ascii_case(POSITION_CLASS) {
            return None;
        }
        Some((only.id()?, only.get("targetname")?.to_owned()))
    }

    /// A targetname no entity has yet for the position `key` of the soundscape entity
    /// `id`: the entity's own name, or `soundscape` and its id, then the key.
    fn position_name(&self, id: u32, key: &str) -> String {
        let owner = self
            .entity(id)
            .and_then(|entity| entity.get("targetname"))
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map_or_else(|| format!("soundscape{id}"), str::to_owned);
        let base = format!("{owner}_{}", key.to_ascii_lowercase());
        let taken = self.targetnames();
        let free = |name: &str| !taken.iter().any(|used| used.eq_ignore_ascii_case(name));
        if free(&base) {
            return base;
        }
        (2u32..)
            .map(|number| format!("{base}_{number}"))
            .find(|name| free(name))
            .unwrap_or(base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picked_position_makes_a_named_target_and_then_moves_it() {
        let mut editor = Editor::default();
        editor.new_map();
        let soundscape = editor.create_entity("env_soundscape", DVec3::ZERO, &[]).unwrap();
        let ids = [soundscape];

        let first = editor
            .place_position(&ids, "position0", DVec3::new(64.0, 0.0, 16.0))
            .unwrap();
        assert!(first.created);
        assert_eq!(first.name, format!("soundscape{soundscape}_position0"));
        assert_eq!(editor.entity(soundscape).unwrap().get("position0"), Some(first.name.as_str()));
        let target = editor.entity(first.entity).unwrap();
        assert_eq!(target.classname(), "info_target");
        assert_eq!(target.get("origin"), Some("64 0 16"));

        let moved = editor
            .place_position(&ids, "position0", DVec3::new(-32.0, 8.0, 0.0))
            .unwrap();
        assert!(!moved.created);
        assert_eq!(moved.entity, first.entity);
        assert_eq!(editor.entity(first.entity).unwrap().get("origin"), Some("-32 8 0"));

        let second = editor.place_position(&ids, "position1", DVec3::ZERO).unwrap();
        assert!(second.created);
        assert_ne!(second.name, first.name);

        assert!(editor.step_history(false).unwrap());
        assert!(editor.entity(second.entity).is_none());
        assert_eq!(editor.entity(soundscape).unwrap().get("position1"), None);

        assert!(editor.place_position(&ids, "radius", DVec3::ZERO).is_err());
        assert!(editor.place_position(&[first.entity], "position0", DVec3::ZERO).is_err());
    }

    #[test]
    fn only_the_eight_positions_of_soundscape_entities_are_position_keys() {
        assert!(is_position_key("env_soundscape", "position0"));
        assert!(is_position_key("env_soundscape_triggerable", "Position7"));
        assert!(!is_position_key("env_soundscape", "position8"));
        assert!(!is_position_key("env_soundscape", "position"));
        assert!(!is_position_key("env_soundscape", "position01"));
        assert!(!is_position_key("env_soundscape_proxy", "position0"));
        assert!(!is_position_key("info_target", "position0"));
    }
}
