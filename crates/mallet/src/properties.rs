//! Reading and editing the keyvalues, flags and outputs of the selected entities. Shared
//! by the Object Properties window and the MCP tools, so both make the same undoable edits.

use std::collections::BTreeSet;

use mallet_content::EntityClass;
use mallet_vmf::edit::Edit;
use mallet_vmf::{Connection, Entity};

use crate::editor::Editor;
use crate::scene::Owner;

/// An output of another entity that targets this one, as shown on the Inputs tab.
#[derive(Debug, Clone, PartialEq)]
pub struct InputLink {
    /// The entity whose output it is.
    pub source: u32,
    /// The source's targetname, or its class when it has none.
    pub source_name: String,
    pub connection: Connection,
}

/// Targets that name entities in special ways rather than by targetname.
const SPECIAL_TARGETS: [&str; 5] = ["!self", "!activator", "!caller", "!player", "!pvsplayer"];

impl Editor {
    /// The entities the properties window edits: every selected entity, and the world
    /// when world brushes are selected, as in Hammer.
    pub fn property_targets(&self) -> Vec<u32> {
        let world = self
            .document
            .as_ref()
            .and_then(|document| document.vmf.world())
            .and_then(Entity::id);
        if self.properties_world {
            return world.into_iter().collect();
        }
        let mut ids = BTreeSet::new();
        for owner in &self.selection {
            match owner {
                Owner::Entity(id) => {
                    ids.insert(*id);
                }
                Owner::Solid(_) => ids.extend(world),
            }
        }
        ids.into_iter().collect()
    }

    /// The studio models the map's entities use, lower case with forward slashes, each
    /// with how many entities use it, sorted.
    pub fn used_models(&self) -> Vec<(String, usize)> {
        let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        let entities = self.document.iter().flat_map(|document| document.vmf.entities());
        for model in entities.filter_map(|entity| entity.get("model")) {
            let model = model.replace('\\', "/").to_ascii_lowercase();
            if model.ends_with(".mdl") {
                *counts.entry(model).or_default() += 1;
            }
        }
        counts.into_iter().collect()
    }

    /// The world or entity with this id.
    pub fn entity(&self, id: u32) -> Option<&Entity> {
        self.document.as_ref()?.vmf.entity_by_id(id)
    }

    /// The FGD class with this name, when game data is loaded.
    pub fn entity_class(&self, classname: &str) -> Option<&EntityClass> {
        self.game_data.as_deref()?.class(classname)
    }

    /// Sets a key on several entities as one undoable step. Entities that already have
    /// the value are left alone.
    pub fn set_key_on(&mut self, ids: &[u32], key: &str, value: &str) -> Result<(), String> {
        let edits: Vec<Edit> = ids
            .iter()
            .filter(|id| self.entity(**id).and_then(|entity| entity.get(key)) != Some(value))
            .map(|id| Edit::SetKey {
                entity: *id,
                key: key.to_owned(),
                value: value.to_owned(),
            })
            .collect();
        self.apply_edits(edits)
    }

    /// Changes the class of several entities as one undoable step. Like Hammer, each
    /// gains the new class's default keyvalues for the keys it does not have yet, so a
    /// brush tied as `func_detail` and turned into a `func_door` gets the door's speed.
    pub fn set_class_on(&mut self, ids: &[u32], classname: &str) -> Result<(), String> {
        let classname = classname.trim();
        if classname.is_empty() {
            return Err("a class name cannot be empty".to_owned());
        }
        let defaults = self.class_defaults(classname);
        let has_fgd = self.entity_class(classname).is_some();
        let mut edits = Vec::new();
        for id in ids {
            let Some(entity) = self.entity(*id) else { continue };
            if entity.classname() != classname {
                edits.push(Edit::SetKey {
                    entity: *id,
                    key: "classname".to_owned(),
                    value: classname.to_owned(),
                });
            }
            if !has_fgd {
                continue;
            }
            for (key, value) in &defaults {
                if entity.get(key).is_none() && !key.eq_ignore_ascii_case("origin") {
                    edits.push(Edit::SetKey {
                        entity: *id,
                        key: key.clone(),
                        value: value.clone(),
                    });
                }
            }
        }
        self.apply_edits(edits)
    }

    /// Removes a key from every entity in `ids` that has it, as one undoable step.
    pub fn remove_key_on(&mut self, ids: &[u32], key: &str) -> Result<(), String> {
        let edits: Vec<Edit> = ids
            .iter()
            .filter(|id| self.entity(**id).is_some_and(|entity| entity.get(key).is_some()))
            .map(|id| Edit::RemoveKey {
                entity: *id,
                key: key.to_owned(),
            })
            .collect();
        self.apply_edits(edits)
    }

    /// Sets or clears one `spawnflags` bit on several entities as one undoable step.
    pub fn set_flag_on(&mut self, ids: &[u32], bit: u32, on: bool) -> Result<(), String> {
        let edits: Vec<Edit> = ids
            .iter()
            .filter_map(|id| {
                let flags = spawnflags(self.entity(*id)?);
                let changed = if on { flags | bit } else { flags & !bit };
                (changed != flags).then(|| Edit::SetKey {
                    entity: *id,
                    key: "spawnflags".to_owned(),
                    value: changed.to_string(),
                })
            })
            .collect();
        self.apply_edits(edits)
    }

    /// Replaces an entity's outputs as one undoable step.
    pub fn set_outputs(&mut self, id: u32, connections: Vec<Connection>) -> Result<(), String> {
        if self
            .entity(id)
            .ok_or_else(|| format!("no entity has id {id}"))?
            .connections()
            == connections
        {
            return Ok(());
        }
        self.apply_edit(&Edit::SetConnections {
            entity: id,
            connections,
        })
    }

    /// Every targetname in the map, sorted and without repeats.
    pub fn targetnames(&self) -> Vec<String> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        let names: BTreeSet<String> = document
            .vmf
            .all_entities()
            .filter_map(|entity| entity.get("targetname"))
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        names.into_iter().collect()
    }

    /// Whether an output target names something: an entity's targetname or class, a
    /// wildcard, or one of the `!` names.
    pub fn target_exists(&self, target: &str) -> bool {
        if target.is_empty()
            || target.ends_with('*')
            || SPECIAL_TARGETS
                .iter()
                .any(|special| special.eq_ignore_ascii_case(target))
        {
            return true;
        }
        self.document.as_ref().is_some_and(|document| {
            document.vmf.all_entities().any(|entity| {
                entity
                    .get("targetname")
                    .is_some_and(|name| name.eq_ignore_ascii_case(target))
                    || entity.classname().eq_ignore_ascii_case(target)
            })
        })
    }

    /// The class of the first entity a target names, for listing its inputs.
    pub fn target_class(&self, target: &str) -> Option<&EntityClass> {
        let document = self.document.as_ref()?;
        let entity = document.vmf.all_entities().find(|entity| {
            entity
                .get("targetname")
                .is_some_and(|name| name.eq_ignore_ascii_case(target))
                || entity.classname().eq_ignore_ascii_case(target)
        })?;
        self.entity_class(entity.classname())
    }

    /// Outputs of other entities that target the entity with this id by its targetname.
    pub fn inputs_to(&self, id: u32) -> Vec<InputLink> {
        let Some(name) = self.entity(id).and_then(|entity| entity.get("targetname")) else {
            return Vec::new();
        };
        if name.is_empty() {
            return Vec::new();
        }
        let Some(document) = &self.document else {
            return Vec::new();
        };
        let mut links = Vec::new();
        for entity in document.vmf.all_entities() {
            let Some(source) = entity.id() else { continue };
            for connection in entity.connections() {
                if connection.target.eq_ignore_ascii_case(name) {
                    links.push(InputLink {
                        source,
                        source_name: entity.get("targetname").unwrap_or(entity.classname()).to_owned(),
                        connection,
                    });
                }
            }
        }
        links
    }

    /// The keyvalues a new entity of a class starts with: every FGD default that is not
    /// empty, with `spawnflags` made of the flags that are on by default. Without an FGD
    /// for the class, lighting classes get the defaults of Source's own FGDs from the
    /// lighting catalogue, so a new light lights; other classes get zero angles.
    pub fn class_defaults(&self, classname: &str) -> Vec<(String, String)> {
        let Some(class) = self.entity_class(classname) else {
            return match mallet_vmf::light::lighting_class(classname) {
                Some(lighting) if !lighting.defaults.is_empty() => lighting
                    .defaults
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
                _ => vec![("angles".to_owned(), "0 0 0".to_owned())],
            };
        };
        class
            .properties
            .iter()
            .filter_map(|property| {
                if property.kind == "flags" {
                    let bits: u32 = property
                        .choices
                        .iter()
                        .filter(|choice| choice.default_on)
                        .filter_map(|choice| choice.value.trim().parse::<u32>().ok())
                        .sum();
                    Some((property.name.clone(), bits.to_string()))
                } else {
                    (!property.default.is_empty()).then(|| (property.name.clone(), property.default.clone()))
                }
            })
            .collect()
    }

    fn apply_edits(&mut self, mut edits: Vec<Edit>) -> Result<(), String> {
        match edits.len() {
            0 => Ok(()),
            1 => self.apply_edit(&edits.remove(0)),
            _ => self.apply_edit(&Edit::Batch(edits)),
        }
    }
}

/// The entity's `spawnflags` as a number, 0 when missing or malformed.
pub fn spawnflags(entity: &Entity) -> u32 {
    entity
        .get("spawnflags")
        .and_then(|flags| flags.trim().parse().ok())
        .unwrap_or(0)
}
