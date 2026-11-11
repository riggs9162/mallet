//! Hammer's Check for Problems (Alt+P): what would make the map fail to compile or play,
//! with a fix for the problems Hammer can fix itself.

use std::collections::{HashMap, HashSet};

use mallet_content::ClassKind;
use mallet_vmf::edit::Edit;
use mallet_vmf::geometry::solid_faces;
use mallet_vmf::Entity;

use crate::editor::Editor;
use crate::scene::Owner;

/// The classes that count as a place for players to start.
const PLAYER_STARTS: &[&str] = &[
    "info_player_start",
    "info_player_deathmatch",
    "info_player_combine",
    "info_player_rebel",
    "info_player_counterterrorist",
    "info_player_terrorist",
    "info_player_teamspawn",
    "info_player_axis",
    "info_player_allies",
];

/// Keys every entity may have although its FGD class does not list them.
const ALWAYS_USED: &[&str] = &["id", "classname", "origin", "angles", "spawnflags", "hammerid"];

/// Names outputs may target that are not entity names.
const SPECIAL_TARGETS_PREFIX: char = '!';

/// The lightmap scale a fix gives faces with none.
const DEFAULT_LIGHTMAP_SCALE: u32 = 16;

/// What kind of problem a check found, which decides what Fix does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProblemKind {
    NoPlayerStart,
    NoClass,
    EmptyBrushEntity,
    InvalidSolid,
    MissingTarget,
    UnusedKeys,
    BadLightmapScale,
    DuplicateId,
}

impl ProblemKind {
    /// What the problem is called in the list.
    pub fn label(self) -> &'static str {
        match self {
            Self::NoPlayerStart => "No player start",
            Self::NoClass => "Entity with no class",
            Self::EmptyBrushEntity => "Brush entity with no brushes",
            Self::InvalidSolid => "Invalid solid structure",
            Self::MissingTarget => "Missing target",
            Self::UnusedKeys => "Unused keyvalues",
            Self::BadLightmapScale => "Invalid lightmap scale",
            Self::DuplicateId => "Duplicate id",
        }
    }

    /// Whether Fix can repair it.
    pub fn fixable(self) -> bool {
        matches!(
            self,
            Self::NoClass | Self::EmptyBrushEntity | Self::InvalidSolid | Self::UnusedKeys | Self::BadLightmapScale
        )
    }
}

/// One problem: its kind, the object it is about, and what exactly is wrong.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub kind: ProblemKind,
    pub object: Option<Owner>,
    pub detail: String,
    /// For unused keyvalues, the keys; for a bad lightmap scale, the side ids.
    pub items: Vec<String>,
}

impl Editor {
    /// Checks the open map the way Hammer's Check for Problems does.
    pub fn check_for_problems(&self) -> Vec<Problem> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        let vmf = &document.vmf;
        let mut problems = Vec::new();
        let names: HashSet<String> = vmf
            .all_entities()
            .flat_map(|entity| [entity.get("targetname"), Some(entity.classname())])
            .flatten()
            .filter(|name| !name.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        let starts = vmf
            .entities()
            .any(|entity| PLAYER_STARTS.iter().any(|class| entity.classname().eq_ignore_ascii_case(class)));
        if !starts {
            problems.push(Problem {
                kind: ProblemKind::NoPlayerStart,
                object: None,
                detail: "Players have nowhere to start; add an info_player_start".to_owned(),
                items: Vec::new(),
            });
        }
        let mut solid_ids: HashMap<u32, usize> = HashMap::new();
        let mut entity_ids: HashMap<u32, usize> = HashMap::new();
        for entity in vmf.all_entities() {
            let world = entity.classname().eq_ignore_ascii_case("worldspawn");
            let owner = entity.id().map(Owner::Entity);
            if let Some(id) = entity.id().filter(|_| !world) {
                *entity_ids.entry(id).or_default() += 1;
            }
            for solid in entity.solids() {
                let solid_owner = if world { solid.id().map(Owner::Solid) } else { owner };
                if let Some(id) = solid.id() {
                    *solid_ids.entry(id).or_default() += 1;
                }
                if solid.sides().all(|side| side.dispinfo().is_none()) && solid_faces(solid).len() < 4 {
                    problems.push(Problem {
                        kind: ProblemKind::InvalidSolid,
                        object: solid_owner,
                        detail: format!("Brush {} does not close a volume", solid.id().unwrap_or(0)),
                        items: solid.id().map(|id| vec![id.to_string()]).unwrap_or_default(),
                    });
                }
                let bad_scales: Vec<String> = solid
                    .sides()
                    .filter(|side| side.lightmap_scale == Some(0))
                    .map(|side| side.id.to_string())
                    .collect();
                if !bad_scales.is_empty() {
                    problems.push(Problem {
                        kind: ProblemKind::BadLightmapScale,
                        object: solid_owner,
                        detail: format!("{} faces of brush {} have a lightmap scale of 0", bad_scales.len(), solid.id().unwrap_or(0)),
                        items: bad_scales,
                    });
                }
            }
            if world {
                continue;
            }
            self.check_entity(entity, owner, &names, &mut problems);
        }
        for (ids, noun) in [(&solid_ids, "brushes"), (&entity_ids, "entities")] {
            let mut duplicates: Vec<(&u32, &usize)> = ids.iter().filter(|(_, count)| **count > 1).collect();
            duplicates.sort();
            for (id, count) in duplicates {
                problems.push(Problem {
                    kind: ProblemKind::DuplicateId,
                    object: None,
                    detail: format!("{count} {noun} share the id {id}"),
                    items: Vec::new(),
                });
            }
        }
        problems
    }

    /// The checks of one entity other than the world.
    fn check_entity(&self, entity: &Entity, owner: Option<Owner>, names: &HashSet<String>, problems: &mut Vec<Problem>) {
        let classname = entity.classname();
        if classname.trim().is_empty() {
            problems.push(Problem {
                kind: ProblemKind::NoClass,
                object: owner,
                detail: format!("Entity {} has no class", entity.id().unwrap_or(0)),
                items: Vec::new(),
            });
            return;
        }
        let class = self.entity_class(classname);
        if class.is_some_and(|class| class.kind == ClassKind::Solid) && entity.solids().next().is_none() {
            problems.push(Problem {
                kind: ProblemKind::EmptyBrushEntity,
                object: owner,
                detail: format!("{classname} {} has no brushes", entity.id().unwrap_or(0)),
                items: Vec::new(),
            });
        }
        let exists = |target: &str| {
            target.is_empty()
                || target.starts_with(SPECIAL_TARGETS_PREFIX)
                || target.ends_with('*')
                || names.contains(&target.to_ascii_lowercase())
        };
        for connection in entity.connections() {
            if !exists(&connection.target) {
                problems.push(Problem {
                    kind: ProblemKind::MissingTarget,
                    object: owner,
                    detail: format!(
                        "{classname} {}'s output {} targets \"{}\", which nothing is called",
                        entity.id().unwrap_or(0),
                        connection.output,
                        connection.target
                    ),
                    items: Vec::new(),
                });
            }
        }
        let Some(class) = class else { return };
        for property in class.properties.iter().filter(|property| property.kind == "target_destination") {
            if let Some(target) = entity.get(&property.name).filter(|target| !exists(target)) {
                problems.push(Problem {
                    kind: ProblemKind::MissingTarget,
                    object: owner,
                    detail: format!(
                        "{classname} {}'s {} names \"{target}\", which nothing is called",
                        entity.id().unwrap_or(0),
                        property.display
                    ),
                    items: Vec::new(),
                });
            }
        }
        let unused: Vec<String> = entity
            .items
            .iter()
            .filter_map(|item| match item {
                mallet_vmf::EntityItem::Key(pair) => Some(pair.key.clone()),
                _ => None,
            })
            .filter(|key| {
                !ALWAYS_USED.iter().any(|always| always.eq_ignore_ascii_case(key)) && class.property(key).is_none()
            })
            .collect();
        if !unused.is_empty() {
            problems.push(Problem {
                kind: ProblemKind::UnusedKeys,
                object: owner,
                detail: format!("{classname} {} has keys its class does not use: {}", entity.id().unwrap_or(0), unused.join(", ")),
                items: unused,
            });
        }
    }

    /// Fixes the problems that can be fixed, as one undo step: entities with no class or
    /// no brushes and invalid brushes are deleted, unused keys removed and lightmap scales
    /// of 0 set to 16. Returns how many problems were fixed.
    pub fn fix_problems(&mut self, problems: &[Problem]) -> Result<usize, String> {
        let mut edits = Vec::new();
        let mut fixed = 0;
        let mut scales: Vec<u32> = Vec::new();
        for problem in problems.iter().filter(|problem| problem.kind.fixable()) {
            match (problem.kind, problem.object) {
                (ProblemKind::NoClass | ProblemKind::EmptyBrushEntity, Some(Owner::Entity(entity))) => {
                    edits.push(Edit::RemoveEntity { entity });
                }
                (ProblemKind::InvalidSolid, _) => {
                    edits.extend(
                        problem
                            .items
                            .iter()
                            .filter_map(|id| id.parse().ok())
                            .map(|solid| Edit::RemoveSolid { solid }),
                    );
                }
                (ProblemKind::UnusedKeys, Some(Owner::Entity(entity))) => {
                    edits.extend(problem.items.iter().map(|key| Edit::RemoveKey {
                        entity,
                        key: key.clone(),
                    }));
                }
                (ProblemKind::BadLightmapScale, _) => {
                    scales.extend(problem.items.iter().filter_map(|id| id.parse::<u32>().ok()));
                }
                _ => continue,
            }
            fixed += 1;
        }
        if !edits.is_empty() {
            self.apply_edit(&Edit::Batch(edits))?;
        }
        if !scales.is_empty() {
            let sides = scales.into_iter().collect();
            self.edit_sides(&sides, None, |side, _| side.lightmap_scale = Some(DEFAULT_LIGHTMAP_SCALE))?;
        }
        self.message = format!("Fixed {fixed} problems");
        Ok(fixed)
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;

    #[test]
    fn an_empty_map_needs_a_player_start_and_missing_targets_are_found() {
        let mut editor = Editor::default();
        editor.new_map();
        let kinds = |editor: &Editor| editor.check_for_problems().iter().map(|problem| problem.kind).collect::<Vec<_>>();
        assert_eq!(kinds(&editor), [ProblemKind::NoPlayerStart]);
        let keys = [("target".to_owned(), "nowhere".to_owned())];
        editor.create_entity("info_player_start", DVec3::ZERO, &[]).unwrap();
        let relay = editor.create_entity("logic_relay", DVec3::ZERO, &keys).unwrap();
        let connection = mallet_vmf::Connection {
            output: "OnTrigger".to_owned(),
            target: "missing_door".to_owned(),
            input: "Open".to_owned(),
            parameter: String::new(),
            delay: 0.0,
            refires: -1,
        };
        editor.set_outputs(relay, vec![connection]).unwrap();
        assert!(kinds(&editor).contains(&ProblemKind::MissingTarget));
        assert!(!kinds(&editor).contains(&ProblemKind::NoPlayerStart));
    }
}
