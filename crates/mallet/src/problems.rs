//! Hammer's Check for Problems (Alt+P): what would make the map fail to compile or play,
//! with a fix for the problems Hammer can fix itself.

use std::collections::{HashMap, HashSet};

use mallet_content::{ClassKind, MaterialLoader, PropTraits};
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

/// Classes the map compiler handles itself, so no game data lists them: VBSP++ turns
/// `func_detail_illusionary` into detail brushes that nothing collides with.
const COMPILER_CLASSES: &[&str] = &["func_detail_illusionary"];

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
    WrongPropClass,
    UnknownClass,
    MissingParent,
    MissingInstance,
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
            Self::WrongPropClass => "Wrong prop class",
            Self::UnknownClass => "Unknown class",
            Self::MissingParent => "Missing parent",
            Self::MissingInstance => "Instance not shown",
        }
    }
}

/// One problem: its kind, the object it is about, and what exactly is wrong.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub kind: ProblemKind,
    pub object: Option<Owner>,
    pub detail: String,
    /// For unused keyvalues, the keys; for a bad lightmap scale, the side ids; for a prop
    /// with the wrong class, the class Fix gives it, when one stands in for its own.
    pub items: Vec<String>,
}

impl Problem {
    /// Whether Fix can repair it. A prop with the wrong class can be fixed when a class
    /// stands in for its own.
    pub fn fixable(&self) -> bool {
        match self.kind {
            ProblemKind::NoClass
            | ProblemKind::EmptyBrushEntity
            | ProblemKind::InvalidSolid
            | ProblemKind::UnusedKeys
            | ProblemKind::BadLightmapScale => true,
            ProblemKind::WrongPropClass => !self.items.is_empty(),
            ProblemKind::NoPlayerStart
            | ProblemKind::MissingTarget
            | ProblemKind::DuplicateId
            | ProblemKind::UnknownClass
            | ProblemKind::MissingParent
            | ProblemKind::MissingInstance => false,
        }
    }

    /// What Fix does about it, where the kind of problem does not say: the class a prop
    /// with the wrong class is given.
    pub fn fix_note(&self) -> Option<String> {
        let class = self
            .items
            .first()
            .filter(|_| self.kind == ProblemKind::WrongPropClass)?;
        Some(format!("Fix makes it a {class}, which keeps the model."))
    }
}

/// What decides which prop classes keep the models a check meets, read once for each
/// model however many props use it.
struct PropModels {
    /// The game's loader, or `None` without a game to read models from.
    loader: Option<MaterialLoader>,
    /// Each model looked at so far, by its path in lower case with forward slashes: its
    /// traits and whether it has a collision model, or `None` when it cannot be read.
    known: HashMap<String, Option<(PropTraits, bool)>>,
}

impl PropModels {
    fn new(loader: Option<MaterialLoader>) -> Self {
        Self {
            loader,
            known: HashMap::new(),
        }
    }

    /// The problem of a prop whose class the map compiler or the game takes its model
    /// from, when `entity` is one, with the class that stands in for its own if there is
    /// one. Only the `prop_` classes have such rules, so no other entity's model is
    /// read. Nothing is said without a game, or about a model that cannot be read.
    fn wrong_class(&mut self, entity: &Entity, owner: Option<Owner>) -> Option<Problem> {
        let classname = entity.classname();
        if !classname.trim().to_ascii_lowercase().starts_with("prop_") {
            return None;
        }
        let model = entity
            .get("model")
            .filter(|model| model.to_ascii_lowercase().ends_with(".mdl"))?;
        let loader = self.loader.as_ref()?;
        let (traits, collision) = self
            .known
            .entry(model.replace('\\', "/").to_ascii_lowercase())
            .or_insert_with(|| Some((loader.model_prop_traits(model)?, loader.model_has_collision(model))))
            .as_ref()?;
        let reason = traits.class_problem(classname, *collision)?;
        Some(Problem {
            kind: ProblemKind::WrongPropClass,
            object: owner,
            detail: format!("{classname} {} uses {model}. {reason}", entity.id().unwrap_or(0)),
            items: traits
                .replacement_class(classname, *collision)
                .map(str::to_owned)
                .into_iter()
                .collect(),
        })
    }
}

impl Editor {
    /// Checks the open map the way Hammer's Check for Problems does, and for props whose
    /// class loses their model, which Hammer leaves to the compile log and the game's
    /// console.
    pub fn check_for_problems(&self) -> Vec<Problem> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        let vmf = &document.vmf;
        let mut problems = Vec::new();
        let mut models = PropModels::new(self.materials.as_ref().map(|materials| materials.loader()));
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
            self.check_entity(entity, owner, &names, &mut models, &mut problems);
        }
        for problem in &self.scene.instance_problems {
            problems.push(Problem {
                kind: ProblemKind::MissingInstance,
                object: Some(Owner::Entity(problem.entity)),
                detail: format!("Instance {} {}", problem.entity, problem.detail),
                items: Vec::new(),
            });
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
    fn check_entity(
        &self,
        entity: &Entity,
        owner: Option<Owner>,
        names: &HashSet<String>,
        models: &mut PropModels,
        problems: &mut Vec<Problem>,
    ) {
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
        let compiler_class = COMPILER_CLASSES.iter().any(|known| known.eq_ignore_ascii_case(classname.trim()));
        if class.is_none() && self.game_data.is_some() && !compiler_class {
            problems.push(Problem {
                kind: ProblemKind::UnknownClass,
                object: owner,
                detail: format!("{classname} {} is not a class the game data knows", entity.id().unwrap_or(0)),
                items: Vec::new(),
            });
        }
        if class.is_some_and(|class| class.kind == ClassKind::Solid) && entity.solids().next().is_none() {
            problems.push(Problem {
                kind: ProblemKind::EmptyBrushEntity,
                object: owner,
                detail: format!("{classname} {} has no brushes", entity.id().unwrap_or(0)),
                items: Vec::new(),
            });
        }
        problems.extend(models.wrong_class(entity, owner));
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
        let parent = entity
            .get("parentname")
            .map(|parent| parent.split(',').next().unwrap_or("").trim())
            .filter(|parent| !exists(parent));
        if let Some(parent) = parent {
            problems.push(Problem {
                kind: ProblemKind::MissingParent,
                object: owner,
                detail: format!(
                    "{classname} {}'s parent is \"{parent}\", which nothing is called",
                    entity.id().unwrap_or(0)
                ),
                items: Vec::new(),
            });
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

    /// The edits that give a prop another class to keep its model: the class change
    /// Object Properties makes, and the removal of each key that still holds a default of
    /// the old class and that the new class does not have, so the fix leaves no unused
    /// keyvalues behind. A key that was given a value of its own stays, and so does every
    /// key when the game data lacks either class.
    fn prop_class_edits(&self, id: u32, classname: &str) -> Vec<Edit> {
        let mut edits = self.class_change_edits(&[id], classname);
        let Some(entity) = self.entity(id) else {
            return edits;
        };
        let (Some(old), Some(new)) = (self.entity_class(entity.classname()), self.entity_class(classname)) else {
            return edits;
        };
        let stale = entity
            .items
            .iter()
            .filter_map(|item| match item {
                mallet_vmf::EntityItem::Key(pair) => Some(pair),
                _ => None,
            })
            .filter(|pair| !ALWAYS_USED.iter().any(|always| always.eq_ignore_ascii_case(&pair.key)))
            .filter(|pair| new.property(&pair.key).is_none())
            .filter(|pair| {
                old.property(&pair.key)
                    .is_some_and(|property| !property.default.is_empty() && property.default == pair.value)
            });
        edits.extend(stale.map(|pair| Edit::RemoveKey {
            entity: id,
            key: pair.key.clone(),
        }));
        edits
    }

    /// Fixes the problems that can be fixed, as one undo step: entities with no class or
    /// no brushes and invalid brushes are deleted, unused keys removed, lightmap scales
    /// of 0 set to 16, and a prop with the wrong class is given the class that stands in
    /// for its own. Returns how many problems were fixed.
    pub fn fix_problems(&mut self, problems: &[Problem]) -> Result<usize, String> {
        let mut edits = Vec::new();
        let mut fixed = 0;
        let mut scales: Vec<u32> = Vec::new();
        for problem in problems.iter().filter(|problem| problem.fixable()) {
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
                (ProblemKind::WrongPropClass, Some(Owner::Entity(entity))) => {
                    let Some(class) = problem.items.first() else { continue };
                    edits.extend(self.prop_class_edits(entity, class));
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
    use serde_json::json;

    use super::*;
    use crate::tools::run;
    use crate::tools::test_game::{json_of, TestGame};

    /// Adds an entity of a class with a model, and returns its id.
    fn place(editor: &mut Editor, classname: &str, model: &str) -> u32 {
        let keys = [("model".to_owned(), model.to_owned())];
        editor.create_entity(classname, DVec3::ZERO, &keys).unwrap()
    }

    /// The props whose class loses their model, with what is said about each.
    fn wrong_classes(editor: &Editor) -> Vec<Problem> {
        let mut problems = editor.check_for_problems();
        problems.retain(|problem| problem.kind == ProblemKind::WrongPropClass);
        problems
    }

    #[test]
    fn props_whose_class_loses_their_model_are_found() {
        let game = TestGame::new("prop-classes");
        game.model("test/drum", true, Some("Metal.Medium"), true);
        game.model("test/lamp", true, None, true);
        game.model("test/citizen", false, None, true);
        let mut editor = game.editor();
        let static_drum = place(&mut editor, "prop_static", "models/test/drum.mdl");
        let dynamic_drum = place(&mut editor, "prop_dynamic", "Models\\Test\\Drum.MDL");
        place(&mut editor, "prop_physics", "models/test/drum.mdl");
        place(&mut editor, "prop_dynamic_override", "models/test/drum.mdl");
        place(&mut editor, "prop_static", "models/test/lamp.mdl");
        let physics_lamp = place(&mut editor, "prop_physics", "models/test/lamp.mdl");
        let static_citizen = place(&mut editor, "prop_static", "models/test/citizen.mdl");
        place(&mut editor, "npc_citizen", "models/test/citizen.mdl");
        place(&mut editor, "prop_static", "models/test/missing.mdl");

        let found = wrong_classes(&editor);
        let props: Vec<Option<Owner>> = found.iter().map(|problem| problem.object).collect();
        let expected = [static_drum, dynamic_drum, physics_lamp, static_citizen].map(|id| Some(Owner::Entity(id)));
        assert_eq!(props, expected);
        assert_eq!(
            found[0].detail,
            format!(
                "prop_static {static_drum} uses models/test/drum.mdl. It has prop data, which makes it a physics \
                 prop, so VBSP leaves it out of the map."
            )
        );
        assert!(
            found[1].detail.contains("prop_dynamic_override takes it"),
            "{}",
            found[1].detail
        );
        assert!(found[2].detail.contains("no prop data"), "{}", found[2].detail);
        assert!(found[3].detail.contains("$staticprop"), "{}", found[3].detail);
    }

    #[test]
    fn fixing_a_prop_gives_it_a_class_that_keeps_its_model() {
        let game = TestGame::new("prop-class-fix");
        game.model("test/drum", true, Some("Metal.Medium"), true);
        game.model("test/crate", true, None, true);
        game.model("test/citizen", false, None, true);
        game.model("test/sign", true, None, false);
        let mut editor = game.editor();
        let props = [
            place(&mut editor, "prop_static", "models/test/drum.mdl"),
            place(&mut editor, "prop_dynamic", "models/test/drum.mdl"),
            place(&mut editor, "prop_physics", "models/test/crate.mdl"),
            place(&mut editor, "prop_static", "models/test/citizen.mdl"),
            place(&mut editor, "prop_physics", "models/test/sign.mdl"),
        ];
        let classes = |editor: &Editor| props.map(|id| editor.entity(id).unwrap().classname().to_owned());

        let found = wrong_classes(&editor);
        let fixes: Vec<&str> = found
            .iter()
            .map(|problem| problem.items.first().map_or("none", String::as_str))
            .collect();
        let fixed = [
            "prop_dynamic_override",
            "prop_dynamic_override",
            "prop_physics_override",
            "prop_dynamic",
        ];
        assert_eq!(fixes[..4], fixed);
        assert_eq!(
            fixes[4], "none",
            "no class stands in for a physics prop without a collision model"
        );
        assert!(found[0].fixable() && !found[4].fixable());
        assert_eq!(
            found[0].fix_note().as_deref(),
            Some("Fix makes it a prop_dynamic_override, which keeps the model.")
        );
        assert_eq!(found[4].fix_note(), None);

        assert_eq!(editor.fix_problems(&found), Ok(4));
        assert_eq!(classes(&editor)[..4], fixed);
        assert_eq!(classes(&editor)[4], "prop_physics");
        let left = wrong_classes(&editor);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].object, Some(Owner::Entity(props[4])));

        assert_eq!(
            editor.step_history(false),
            Ok(true),
            "the whole fix is one step to undo"
        );
        let before = [
            "prop_static",
            "prop_dynamic",
            "prop_physics",
            "prop_static",
            "prop_physics",
        ];
        assert_eq!(classes(&editor), before);
    }

    #[test]
    fn fixing_a_prop_drops_the_defaults_its_new_class_has_no_use_for() {
        let classes = "@PointClass = prop_static : \"A static prop.\" [\n\
            model(studio) : \"Model\"\n\
            solid(integer) : \"Collisions\" : 6\n\
            screenspacefade(integer) : \"Screen space fade\" : 0\n\
            ]\n\
            @PointClass = prop_dynamic_override : \"A dynamic prop.\" [\n\
            model(studio) : \"Model\"\n\
            solid(integer) : \"Collisions\" : 6\n\
            health(integer) : \"Health\" : 0\n\
            ]\n";
        let game = TestGame::new("prop-class-keys");
        game.model("test/drum", true, Some("Metal.Medium"), true);
        let mut editor = game.editor();
        editor.game_data = Some(std::sync::Arc::new(mallet_content::GameData::parse(classes).unwrap()));
        let plain = place(&mut editor, "prop_static", "models/test/drum.mdl");
        let tuned = place(&mut editor, "prop_static", "models/test/drum.mdl");
        editor.set_key_on(&[tuned], "screenspacefade", "1").unwrap();
        editor.set_key_on(&[tuned], "targetname", "drum").unwrap();
        assert_eq!(editor.entity(plain).unwrap().get("screenspacefade"), Some("0"));

        let found = wrong_classes(&editor);
        assert_eq!(editor.fix_problems(&found), Ok(2));
        let keys = |id: u32| {
            let entity = editor.entity(id).unwrap();
            ["classname", "screenspacefade", "solid", "health", "targetname"].map(|key| entity.get(key))
        };
        let class = Some("prop_dynamic_override");
        assert_eq!(keys(plain), [class, None, Some("6"), Some("0"), None]);
        assert_eq!(keys(tuned), [class, Some("1"), Some("6"), Some("0"), Some("drum")]);
        let unused: Vec<Option<Owner>> = editor
            .check_for_problems()
            .iter()
            .filter(|problem| problem.kind == ProblemKind::UnusedKeys)
            .map(|problem| problem.object)
            .collect();
        assert_eq!(
            unused,
            [Some(Owner::Entity(tuned))],
            "only keys with a value of their own are left"
        );
    }

    #[test]
    fn agents_are_told_the_fix_and_can_ask_for_it() {
        let game = TestGame::new("prop-class-tool");
        game.model("test/drum", true, Some("Metal.Medium"), true);
        let mut editor = game.editor();
        let drum = place(&mut editor, "prop_static", "models/test/drum.mdl");
        let wrong = |listed: &serde_json::Value| -> Vec<serde_json::Value> {
            let problems = listed["problems"].as_array().unwrap().iter();
            problems
                .filter(|entry| entry["kind"] == "Wrong prop class")
                .cloned()
                .collect()
        };

        let listed = wrong(&json_of(&run(&mut editor, "check_for_problems", &json!({}))));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["object"]["entity"], drum);
        assert_eq!(listed[0]["fixable"], true);
        assert_eq!(
            listed[0]["fix"],
            "Fix makes it a prop_dynamic_override, which keeps the model."
        );

        let fixed = json_of(&run(&mut editor, "check_for_problems", &json!({ "fix": true })));
        assert!(wrong(&fixed).is_empty());
        assert_eq!(editor.entity(drum).unwrap().classname(), "prop_dynamic_override");
    }

    #[test]
    fn prop_classes_are_not_judged_without_a_game() {
        let mut editor = Editor::default();
        editor.new_map();
        place(&mut editor, "prop_static", "models/test/drum.mdl");
        assert!(wrong_classes(&editor).is_empty());
    }

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
