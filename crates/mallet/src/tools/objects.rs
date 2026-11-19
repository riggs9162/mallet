//! The tools that find a map's objects in bulk, so an agent asks one question instead of
//! reading every object: selecting by what objects are and where they are, listing
//! entities and brushes with filters, setting many keys at once, and tracing lines
//! against the brushes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use glam::DVec3;
use mallet_vmf::geometry::Bounds;
use mallet_vmf::Entity;
use serde_json::{json, Value};

use super::{id_list, optional_flag, optional_numbers, optional_string, tool, whole_number, DEFAULT_LIST_LIMIT};
use crate::editor::{Editor, SelectMode};
use crate::mcp::ToolResult;
use crate::scene::{Owner, WireMesh};
use crate::sound::learn_solid_materials;

/// Selections of more objects of a kind than this give their ids as ranges.
const MAX_LISTED_IDS: usize = 100;
/// How many brushes `list_solids` returns when no limit is given.
const DEFAULT_SOLIDS: u64 = 100;
/// The most lines `trace_line` traces at once.
const MAX_RAYS: usize = 256;
/// How far a line given by a direction is traced when no distance is given, in units:
/// further than any map is across.
const DEFAULT_REACH: f64 = 65536.0;

/// The `tools/list` entries of the tools this module adds.
pub fn definitions() -> Vec<Value> {
    let vec3 = json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 });
    let area = json!({
        "type": "object",
        "properties": { "min": vec3, "max": vec3 },
        "required": ["min", "max"],
        "description": "Only objects inside this box, or with touching, objects that reach into it."
    });
    vec![
        tool(
            "list_solids",
            "List brushes, of the world and of brush entities, with their id, the entity they belong to, the \
             box around them, their face count and their materials: all of them, or those of one entity, with \
             a material, in a box or in the selection. Hidden brushes are left out.",
            json!({
                "entity": { "type": "integer", "description": "Only brushes of this entity; the world is usually 1." },
                "material": { "type": "string", "description": "Only brushes with a face of this material; * stands for any text, as in tools/*." },
                "box": area,
                "touching": { "type": "boolean", "description": "With box, also brushes that only reach into it. Default false." },
                "selected_only": { "type": "boolean", "description": "Only selected brushes and the brushes of selected entities." },
                "limit": { "type": "integer", "minimum": 1, "description": "Default 100." },
                "offset": { "type": "integer", "minimum": 0, "description": "How many matching brushes to skip; default 0." }
            }),
            &[],
        ),
        tool(
            "set_entity_keys",
            "Set several keys on several entities as one undo step: on the entities with the given ids, or on \
             the selected entities when none are given. A key with a string value is set, added if missing; a \
             key with null is removed. Returns how many keyvalues changed.",
            json!({
                "ids": { "type": "array", "items": { "type": "integer" }, "description": "Entity ids; default the selected entities." },
                "keys": {
                    "type": "object",
                    "additionalProperties": { "type": ["string", "null"] },
                    "description": "The keyvalues to set, such as {\"skin\": \"2\", \"targetname\": null}."
                }
            }),
            &["keys"],
        ),
        tool(
            "trace_line",
            "Trace lines through the map and get the first brush face each meets: the point, how far along \
             the line it is, the face's outward normal, side id, brush id, entity and material. Use it to find \
             the floor under a point before placing something (direction [0, 0, -1]), the ceiling, the wall a \
             prop should stand against, or whether two points see each other. A line runs from from to to, or \
             from from along direction for distance units. Like a player or a bullet it passes through \
             triggers, clips, hints and other tool brushes; point entities, props and overlays do not stop it \
             either.",
            json!({
                "rays": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_RAYS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "from": vec3,
                            "to": vec3,
                            "direction": vec3,
                            "distance": { "type": "number", "exclusiveMinimum": 0, "description": "How far to trace along direction; default to the end of the map." }
                        },
                        "required": ["from"]
                    }
                }
            }),
            &["rays"],
        ),
    ]
}

/// Whether `text` matches `pattern`, ignoring case and which way slashes lean. A `*` in
/// the pattern stands for any text, so `trigger_*` matches by prefix and `*door*`
/// anywhere; without one the whole text has to match.
pub(super) fn matches_pattern(pattern: &str, text: &str) -> bool {
    let fold = |text: &str| text.trim().replace('\\', "/").to_ascii_lowercase();
    let (pattern, text) = (fold(pattern), fold(text));
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return text.is_empty();
    };
    if !pattern.contains('*') {
        return text == pattern;
    }
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    for (index, part) in parts.iter().enumerate() {
        if index + 1 == parts.len() {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

/// A box objects are looked for in.
struct Area {
    bounds: Bounds,
    /// Whether objects that only reach into the box count too.
    touching: bool,
}

impl Area {
    /// The `box` and `touching` arguments, when a box is given.
    fn from_args(args: &Value) -> Result<Option<Self>, String> {
        let area = match args.get("box") {
            None | Some(Value::Null) => return Ok(None),
            Some(area) => area,
        };
        let corner = |key: &str| {
            optional_numbers::<3>(area, key)?
                .map(DVec3::from_array)
                .ok_or_else(|| format!("\"box\" needs \"{key}\" as [x, y, z]"))
        };
        let (a, b) = (corner("min")?, corner("max")?);
        Ok(Some(Self {
            bounds: Bounds {
                min: a.min(b),
                max: a.max(b),
            },
            touching: optional_flag(args, "touching")? == Some(true),
        }))
    }

    fn takes(&self, bounds: &Bounds) -> bool {
        if bounds.is_empty() {
            return false;
        }
        if self.touching {
            bounds.min.cmple(self.bounds.max).all() && bounds.max.cmpge(self.bounds.min).all()
        } else {
            bounds.min.cmpge(self.bounds.min).all() && bounds.max.cmple(self.bounds.max).all()
        }
    }
}

/// The box around each object the views show, by owner. An entity of several brushes
/// has the box around all of them.
fn owner_bounds(editor: &Editor) -> BTreeMap<Owner, Bounds> {
    let mut bounds: BTreeMap<Owner, Bounds> = BTreeMap::new();
    for mesh in &editor.scene.meshes {
        if let Some(owner) = mesh.owner.filter(|_| !mesh.geometry_bounds.is_empty()) {
            bounds
                .entry(owner)
                .or_insert(Bounds::EMPTY)
                .add_bounds(&mesh.geometry_bounds);
        }
    }
    bounds
}

/// Whether a mesh is a brush with a face of a material that matches `pattern`.
fn has_material(mesh: &WireMesh, pattern: &str) -> bool {
    mesh.faces
        .iter()
        .filter(|face| face.side.is_some())
        .filter_map(|face| face.material.as_deref())
        .any(|material| matches_pattern(pattern, material))
}

/// What the `select` tool's filters ask for.
struct ObjectFilter {
    all: bool,
    classname: Option<String>,
    material: Option<String>,
    area: Option<Area>,
}

impl ObjectFilter {
    /// The filters among the arguments, when any is given.
    fn from_args(args: &Value) -> Result<Option<Self>, String> {
        let filter = Self {
            all: optional_flag(args, "all")? == Some(true),
            classname: optional_string(args, "classname")?.map(str::to_owned),
            material: optional_string(args, "material")?.map(str::to_owned),
            area: Area::from_args(args)?,
        };
        let any = filter.all || filter.classname.is_some() || filter.material.is_some() || filter.area.is_some();
        Ok(any.then_some(filter))
    }

    /// Every object the views show that passes all the filters.
    fn owners(&self, editor: &Editor) -> Vec<Owner> {
        let mut owners: BTreeSet<Owner> = match &self.area {
            Some(area) => owner_bounds(editor)
                .into_iter()
                .filter(|(_, bounds)| area.takes(bounds))
                .map(|(owner, _)| owner)
                .collect(),
            None => editor.scene.owners(),
        };
        if let Some(pattern) = &self.classname {
            owners.retain(|owner| match owner {
                Owner::Entity(id) => editor
                    .entity(*id)
                    .is_some_and(|entity| matches_pattern(pattern, entity.classname())),
                Owner::Solid(_) => false,
            });
        }
        if let Some(pattern) = &self.material {
            let textured: BTreeSet<Owner> = editor
                .scene
                .meshes
                .iter()
                .filter(|mesh| !mesh.decal && has_material(mesh, pattern))
                .filter_map(|mesh| mesh.owner)
                .collect();
            owners.retain(|owner| textured.contains(owner));
        }
        owners.into_iter().collect()
    }
}

/// The `select` tool: the ids given, and everything the filters find, change the
/// selection as `mode` says.
pub(super) fn select(editor: &mut Editor, args: &Value) -> Result<ToolResult, String> {
    let mode = match optional_string(args, "mode")?.unwrap_or("replace") {
        "replace" => SelectMode::Replace,
        "add" => SelectMode::Add,
        "remove" => SelectMode::Remove,
        "toggle" => SelectMode::Toggle,
        other => return Err(format!("unknown mode \"{other}\"")),
    };
    let mut owners: Vec<Owner> = id_list(args, "solids")?.into_iter().map(Owner::Solid).collect();
    owners.extend(id_list(args, "entities")?.into_iter().map(Owner::Entity));
    if let Some(filter) = ObjectFilter::from_args(args)? {
        owners.extend(filter.owners(editor));
    }
    editor.select(owners, mode);
    Ok(ToolResult::json(&selection_json(editor)))
}

/// Ids in rising order as ranges: a number for an id on its own and `[first, last]` for
/// a run of ids.
fn id_ranges(ids: &[u32]) -> Vec<Value> {
    let mut ranges: Vec<Value> = Vec::new();
    let mut run: Option<(u32, u32)> = None;
    let close = |run: (u32, u32), ranges: &mut Vec<Value>| {
        ranges.push(if run.0 == run.1 {
            json!(run.0)
        } else {
            json!([run.0, run.1])
        });
    };
    for id in ids {
        run = match run {
            Some((first, last)) if last.checked_add(1) == Some(*id) => Some((first, *id)),
            Some(ended) => {
                close(ended, &mut ranges);
                Some((*id, *id))
            }
            None => Some((*id, *id)),
        };
    }
    if let Some(ended) = run {
        close(ended, &mut ranges);
    }
    ranges
}

/// What is selected, for the tools that answer with the selection.
pub(super) fn selection_json(editor: &Editor) -> Value {
    let mut solids = Vec::new();
    let mut entities = Vec::new();
    for owner in &editor.selection {
        match *owner {
            Owner::Solid(id) => solids.push(id),
            Owner::Entity(id) => entities.push(id),
        }
    }
    let bounds = editor.scene.selection_bounds();
    let mut selection = json!({
        "solid_count": solids.len(),
        "entity_count": entities.len(),
        "bounds": (!bounds.is_empty()).then(|| json!({
            "min": bounds.min.to_array(),
            "max": bounds.max.to_array(),
        })),
        "pivot": (!bounds.is_empty()).then(|| editor.pivot().to_array()),
        "pivot_placed": editor.overlay.pivot.is_some(),
        "handles": editor.overlay.handle_mode.label().to_ascii_lowercase(),
    });
    for (ids, list, ranges) in [
        (&solids, "solids", "solid_ranges"),
        (&entities, "entities", "entity_ranges"),
    ] {
        if ids.len() <= MAX_LISTED_IDS {
            selection[list] = json!(ids);
            continue;
        }
        let runs = id_ranges(ids);
        if runs.len() <= MAX_LISTED_IDS {
            selection[ranges] = Value::Array(runs);
        } else {
            selection["ids_left_out"] = json!(
                "Too many ids to list; page through them with list_entities or list_solids and selected_only."
            );
        }
    }
    selection
}

fn entity_summary(entity: &Entity, keys: &[String]) -> Value {
    let mut summary = json!({
        "id": entity.id(),
        "classname": entity.classname(),
        "targetname": entity.get("targetname"),
        "origin": entity.origin().map(|origin| origin.to_array()),
        "solids": entity.solids().count(),
    });
    if !keys.is_empty() {
        let values: serde_json::Map<String, Value> =
            keys.iter().map(|key| (key.clone(), json!(entity.get(key)))).collect();
        summary["keys"] = Value::Object(values);
    }
    summary
}

/// The `list_entities` tool.
pub(super) fn list_entities(editor: &Editor, args: &Value) -> Result<Value, String> {
    let classname = optional_string(args, "classname")?;
    let targetname = optional_string(args, "targetname")?;
    let limit = whole_number(args, "limit")?.unwrap_or(DEFAULT_LIST_LIMIT).max(1) as usize;
    let offset = whole_number(args, "offset")?.unwrap_or(0) as usize;
    let keys = super::string_list(args, "keys")?.unwrap_or_default();
    let summary = optional_flag(args, "summary")? == Some(true);
    let selected_only = optional_flag(args, "selected_only")? == Some(true);
    let wanted: Vec<(String, String)> = match args.get("where") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Object(pairs)) => pairs
            .iter()
            .map(|(key, value)| {
                let value = value
                    .as_str()
                    .ok_or_else(|| format!("\"where\" must give \"{key}\" a string value"))?;
                Ok((key.clone(), value.to_owned()))
            })
            .collect::<Result<_, String>>()?,
        Some(_) => return Err("\"where\" must be an object of keys and the values to look for".to_owned()),
    };
    let area = Area::from_args(args)?;
    let boxes = if area.is_some() {
        owner_bounds(editor)
    } else {
        BTreeMap::new()
    };
    let vmf = &editor.document.as_ref().ok_or("no map is open")?.vmf;
    let matching: Vec<&Entity> = vmf
        .all_entities()
        .filter(|entity| classname.is_none_or(|pattern| matches_pattern(pattern, entity.classname())))
        .filter(|entity| {
            targetname.is_none_or(|pattern| matches_pattern(pattern, entity.get("targetname").unwrap_or_default()))
        })
        .filter(|entity| {
            wanted
                .iter()
                .all(|(key, pattern)| entity.get(key).is_some_and(|value| matches_pattern(pattern, value)))
        })
        .filter(|entity| {
            let owner = entity.id().map(Owner::Entity);
            let in_area = area.as_ref().is_none_or(|area| {
                owner
                    .and_then(|owner| boxes.get(&owner))
                    .is_some_and(|bounds| area.takes(bounds))
            });
            let selected = !selected_only || owner.is_some_and(|owner| editor.selection.contains(&owner));
            in_area && selected
        })
        .collect();
    if summary {
        let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
        for entity in &matching {
            *classes.entry(entity.classname()).or_default() += 1;
        }
        return Ok(json!({ "total": matching.len(), "classes": classes }));
    }
    let entities: Vec<Value> = matching
        .iter()
        .skip(offset)
        .take(limit)
        .map(|entity| entity_summary(entity, &keys))
        .collect();
    let returned = entities.len();
    let mut listed = json!({
        "total": matching.len(),
        "returned": returned,
        "entities": entities,
    });
    if offset + returned < matching.len() {
        listed["next_offset"] = json!(offset + returned);
    }
    Ok(listed)
}

/// The `list_solids` tool.
pub(super) fn list_solids(editor: &Editor, args: &Value) -> Result<Value, String> {
    let vmf = &editor.document.as_ref().ok_or("no map is open")?.vmf;
    let world = vmf.world().and_then(Entity::id);
    let entity = whole_number(args, "entity")?.map(|id| id as u32);
    let material = optional_string(args, "material")?;
    let area = Area::from_args(args)?;
    let selected_only = optional_flag(args, "selected_only")? == Some(true);
    let limit = whole_number(args, "limit")?.unwrap_or(DEFAULT_SOLIDS).max(1) as usize;
    let offset = whole_number(args, "offset")?.unwrap_or(0) as usize;
    let owner_of = |mesh: &WireMesh| match mesh.owner {
        Some(Owner::Entity(id)) => Some(id),
        Some(Owner::Solid(_)) => world,
        None => None,
    };
    let matching: Vec<&WireMesh> = editor
        .scene
        .meshes
        .iter()
        .filter(|mesh| mesh.solid.is_some() && !mesh.decal)
        .filter(|mesh| entity.is_none_or(|wanted| owner_of(mesh) == Some(wanted)))
        .filter(|mesh| material.is_none_or(|pattern| has_material(mesh, pattern)))
        .filter(|mesh| area.as_ref().is_none_or(|area| area.takes(&mesh.geometry_bounds)))
        .filter(|mesh| !selected_only || mesh.selected)
        .collect();
    let solids: Vec<Value> = matching
        .iter()
        .skip(offset)
        .take(limit)
        .map(|mesh| {
            let sides: Vec<&str> = mesh
                .faces
                .iter()
                .filter(|face| face.side.is_some())
                .filter_map(|face| face.material.as_deref())
                .collect();
            let materials: BTreeSet<&str> = sides.iter().copied().collect();
            json!({
                "solid": mesh.solid,
                "entity": owner_of(mesh),
                "min": mesh.geometry_bounds.min.to_array(),
                "max": mesh.geometry_bounds.max.to_array(),
                "faces": sides.len(),
                "materials": materials,
            })
        })
        .collect();
    let returned = solids.len();
    let mut listed = json!({
        "total": matching.len(),
        "returned": returned,
        "solids": solids,
    });
    if offset + returned < matching.len() {
        listed["next_offset"] = json!(offset + returned);
    }
    Ok(listed)
}

/// The `set_entity_keys` tool.
pub(super) fn set_entity_keys(editor: &mut Editor, args: &Value) -> Result<ToolResult, String> {
    let pairs = args
        .get("keys")
        .and_then(Value::as_object)
        .filter(|keys| !keys.is_empty())
        .ok_or("\"keys\" must be an object of keys and their values, or null to remove a key")?;
    let mut keys: Vec<(String, Option<String>)> = Vec::new();
    for (key, value) in pairs {
        let value = match value {
            Value::Null => None,
            Value::String(text) => Some(text.clone()),
            _ => {
                return Err(format!(
                    "the value of \"{key}\" must be a string, or null to remove the key"
                ))
            }
        };
        keys.push((key.clone(), value));
    }
    let mut ids = id_list(args, "ids")?;
    if args.get("ids").is_none_or(Value::is_null) {
        ids = editor
            .selection
            .iter()
            .filter_map(|owner| match owner {
                Owner::Entity(id) => Some(*id),
                Owner::Solid(_) => None,
            })
            .collect();
    }
    if ids.is_empty() {
        return Err("give the entities' \"ids\", or select entities first".to_owned());
    }
    let changed = editor.set_keys_on(&ids, &keys)?;
    Ok(ToolResult::json(
        &json!({ "entities": ids.len(), "keyvalues_changed": changed }),
    ))
}

/// The `trace_line` tool.
pub(super) fn trace_line(editor: &mut Editor, args: &Value) -> Result<ToolResult, String> {
    let rays = args
        .get("rays")
        .and_then(Value::as_array)
        .filter(|rays| !rays.is_empty() && rays.len() <= MAX_RAYS)
        .ok_or_else(|| {
            format!("\"rays\" must be 1 to {MAX_RAYS} lines, each with \"from\" and \"to\" or \"direction\"")
        })?;
    let mut lines: Vec<(DVec3, DVec3, f64)> = Vec::with_capacity(rays.len());
    for (index, ray) in rays.iter().enumerate() {
        let wrong = |what: &str| format!("ray {index}: {what}");
        let from = optional_numbers::<3>(ray, "from")
            .map_err(|error| wrong(&error))?
            .map(DVec3::from_array)
            .ok_or_else(|| wrong("\"from\" is required"))?;
        let to = optional_numbers::<3>(ray, "to")
            .map_err(|error| wrong(&error))?
            .map(DVec3::from_array);
        let direction = optional_numbers::<3>(ray, "direction")
            .map_err(|error| wrong(&error))?
            .map(DVec3::from_array);
        let (direction, reach) = match (to, direction) {
            (Some(to), None) => (to - from, (to - from).length()),
            (None, Some(direction)) => {
                let reach = match ray.get("distance") {
                    None | Some(Value::Null) => DEFAULT_REACH,
                    Some(distance) => distance
                        .as_f64()
                        .filter(|distance| *distance > 0.0)
                        .ok_or_else(|| wrong("\"distance\" must be a number above zero"))?,
                };
                (direction, reach)
            }
            _ => return Err(wrong("give either \"to\" or \"direction\"")),
        };
        let direction = direction
            .try_normalize()
            .ok_or_else(|| wrong("the line has no length"))?;
        lines.push((from, direction, reach));
    }
    if editor.document.is_none() {
        return Err("no map is open".to_owned());
    }
    let mut solid = HashMap::new();
    learn_solid_materials(&mut solid, &editor.scene, editor.materials.as_mut());
    let scene = &editor.scene;
    let world = editor
        .document
        .as_ref()
        .and_then(|document| document.vmf.world())
        .and_then(Entity::id);
    let stops = |mesh: &WireMesh| {
        let entity_solid = match mesh.owner {
            Some(Owner::Entity(entity)) => !scene.sounds.non_solid.contains(&entity),
            _ => true,
        };
        entity_solid
            && mesh.faces.iter().any(|face| {
                face.material
                    .as_deref()
                    .is_none_or(|name| solid.get(&name.to_ascii_lowercase()).copied().unwrap_or(true))
            })
    };
    let round = |value: f64| (value * 1000.0).round() / 1000.0 + 0.0;
    let hits: Vec<Value> = lines
        .iter()
        .map(|(from, direction, reach)| {
            let hit = scene
                .surface_face_where(*from, *direction, stops)
                .filter(|(hit, _)| hit.point.distance(*from) <= *reach + 1e-6);
            let Some((hit, normal)) = hit else {
                return Value::Null;
            };
            let mesh = scene.meshes.iter().find(|mesh| mesh.solid == Some(hit.solid));
            let face = mesh.and_then(|mesh| mesh.faces.iter().find(|face| face.side == Some(hit.side)));
            json!({
                "point": hit.point.to_array().map(round),
                "distance": round(hit.point.distance(*from)),
                "normal": normal.to_array().map(round),
                "side": hit.side,
                "solid": hit.solid,
                "entity": mesh.and_then(|mesh| match mesh.owner {
                    Some(Owner::Entity(id)) => Some(id),
                    _ => world,
                }),
                "material": face.and_then(|face| face.material.clone()),
            })
        })
        .collect();
    Ok(ToolResult::json(&json!({ "hits": hits })))
}

#[cfg(test)]
mod tests {
    use glam::DVec3;
    use serde_json::json;

    use super::super::run;
    use super::super::test_game::{error_of, json_of};
    use super::*;

    /// A map with a floor, a wall of another material, two props and a light.
    fn editor() -> (Editor, [u32; 2], [u32; 3]) {
        let mut editor = Editor::default();
        editor.new_map();
        editor.material = "CONCRETE/FLOOR".to_owned();
        let floor = editor
            .create_block(DVec3::new(-256.0, -256.0, -16.0), DVec3::new(256.0, 256.0, 0.0))
            .unwrap();
        editor.material = "brick/wall".to_owned();
        let wall = editor
            .create_block(DVec3::new(240.0, -256.0, 0.0), DVec3::new(256.0, 256.0, 128.0))
            .unwrap();
        let model = |name: &str| [("model".to_owned(), format!("models/{name}.mdl"))];
        let near = editor
            .create_entity("prop_static", DVec3::new(0.0, 0.0, 16.0), &model("crate"))
            .unwrap();
        let far = editor
            .create_entity("prop_static", DVec3::new(1024.0, 0.0, 16.0), &model("barrel"))
            .unwrap();
        let light = editor.create_entity("light", DVec3::new(0.0, 0.0, 96.0), &[]).unwrap();
        editor.clear_selection();
        (editor, [floor, wall], [near, far, light])
    }

    #[test]
    fn patterns_match_whole_names_or_around_stars() {
        assert!(matches_pattern("prop_static", "PROP_STATIC"));
        assert!(!matches_pattern("prop", "prop_static"));
        assert!(matches_pattern("prop_*", "prop_static"));
        assert!(matches_pattern("*door*", "func_door_rotating"));
        assert!(matches_pattern("tools/*", "TOOLS\\TOOLSNODRAW"));
        assert!(matches_pattern("*_a*c", "x_abc"));
        assert!(!matches_pattern("*_a*c", "x_abcd"));
        assert!(matches_pattern("*", ""));
        assert!(matches_pattern("", ""));
    }

    #[test]
    fn objects_are_selected_by_class_material_and_place() {
        let (mut editor, [floor, wall], [near, far, light]) = editor();
        let all = json_of(&run(&mut editor, "select", &json!({ "all": true })));
        assert_eq!(
            (all["solid_count"].as_u64(), all["entity_count"].as_u64()),
            (Some(2), Some(3))
        );
        assert_eq!(all["solids"], json!([floor, wall]));

        let props = json_of(&run(&mut editor, "select", &json!({ "classname": "prop_*" })));
        assert_eq!(props["entities"], json!([near, far]));
        assert_eq!(props["solid_count"], 0);

        let bricks = json_of(&run(&mut editor, "select", &json!({ "material": "BRICK/*" })));
        assert_eq!(bricks["solids"], json!([wall]));

        let area = json!({ "min": [-300, -300, -64], "max": [300, 300, 200] });
        let inside = json_of(&run(&mut editor, "select", &json!({ "box": area })));
        assert_eq!(inside["solids"], json!([floor, wall]));
        assert_eq!(inside["entities"], json!([near, light]));

        let both = json!({ "box": area, "classname": "prop_static", "mode": "remove" });
        let left = json_of(&run(&mut editor, "select", &both));
        assert_eq!(left["entities"], json!([light]), "filters narrow each other down");

        let slab = json!({ "min": [-64, -64, -8], "max": [64, 64, 8] });
        let none = json_of(&run(&mut editor, "select", &json!({ "box": slab })));
        assert_eq!(none["solid_count"], 0, "the floor only reaches into the box");
        let touched = json_of(&run(&mut editor, "select", &json!({ "box": slab, "touching": true })));
        assert_eq!(touched["solids"], json!([floor]));

        let added = json_of(&run(
            &mut editor,
            "select",
            &json!({ "entities": [far], "mode": "add" }),
        ));
        assert_eq!(
            added["entities"],
            json!([near, far]),
            "the prop that reaches into the box stays"
        );
        assert_eq!(added["solids"], json!([floor]));
    }

    #[test]
    fn large_selections_give_their_ids_as_ranges() {
        let mut ids: Vec<u32> = (10..=160).collect();
        ids.extend([200, 300, 301]);
        assert_eq!(id_ranges(&ids), [json!([10, 160]), json!(200), json!([300, 301])]);

        let mut editor = Editor::default();
        editor.new_map();
        for step in 0..=MAX_LISTED_IDS {
            let x = step as f64 * 64.0;
            editor
                .create_block(DVec3::new(x, 0.0, 0.0), DVec3::new(x + 32.0, 32.0, 32.0))
                .unwrap();
        }
        let selection = json_of(&run(&mut editor, "select", &json!({ "all": true })));
        assert_eq!(selection["solid_count"], MAX_LISTED_IDS + 1);
        assert!(selection.get("solids").is_none());
        assert_eq!(selection["solid_ranges"].as_array().unwrap().len(), 1);
        assert_eq!(selection["entities"], json!([]));
        assert!(selection.get("ids_left_out").is_none());

        for step in 0..=MAX_LISTED_IDS {
            let origin = DVec3::new(step as f64 * 64.0, 256.0, 0.0);
            editor.create_entity("info_target", origin, &[]).unwrap();
            editor
                .create_block(origin + DVec3::Y * 256.0, origin + DVec3::new(32.0, 288.0, 32.0))
                .unwrap();
        }
        let scattered = json_of(&run(&mut editor, "select", &json!({ "classname": "info_target" })));
        assert_eq!(scattered["entity_count"], MAX_LISTED_IDS + 1);
        assert!(scattered.get("entities").is_none() && scattered.get("entity_ranges").is_none());
        assert!(scattered["ids_left_out"].as_str().unwrap().contains("selected_only"));
    }

    #[test]
    fn entities_are_listed_by_filters_and_counted_by_class() {
        let (mut editor, _, [near, far, light]) = editor();
        let props = json_of(&run(
            &mut editor,
            "list_entities",
            &json!({ "classname": "prop_static", "keys": ["model", "skin"] }),
        ));
        assert_eq!(props["total"], 2);
        assert_eq!(
            props["entities"][0]["keys"],
            json!({ "model": "models/crate.mdl", "skin": null })
        );

        let barrels = json_of(&run(
            &mut editor,
            "list_entities",
            &json!({ "where": { "model": "*barrel*" } }),
        ));
        assert_eq!(barrels["entities"][0]["id"], far);

        let area = json!({ "min": [-64, -64, 0], "max": [64, 64, 128] });
        let here = json_of(&run(&mut editor, "list_entities", &json!({ "box": area })));
        let ids: Vec<u64> = here["entities"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entity| entity["id"].as_u64())
            .collect();
        assert_eq!(ids, [u64::from(near), u64::from(light)]);

        let counted = json_of(&run(&mut editor, "list_entities", &json!({ "summary": true })));
        assert_eq!(counted["total"], 4);
        assert_eq!(
            counted["classes"],
            json!({ "light": 1, "prop_static": 2, "worldspawn": 1 })
        );

        let paged = json_of(&run(&mut editor, "list_entities", &json!({ "limit": 1, "offset": 1 })));
        assert_eq!(
            (paged["returned"].as_u64(), paged["next_offset"].as_u64()),
            (Some(1), Some(2))
        );

        run(&mut editor, "select", &json!({ "entities": [light] }));
        let selected = json_of(&run(&mut editor, "list_entities", &json!({ "selected_only": true })));
        assert_eq!(selected["entities"][0]["classname"], "light");
    }

    #[test]
    fn brushes_are_listed_with_their_boxes_and_materials() {
        let (mut editor, [floor, wall], _) = editor();
        let all = json_of(&run(&mut editor, "list_solids", &json!({})));
        assert_eq!(all["total"], 2);
        assert_eq!(all["solids"][0]["solid"], floor);
        assert_eq!(all["solids"][0]["min"], json!([-256.0, -256.0, -16.0]));
        assert_eq!(all["solids"][0]["faces"], 6);
        assert_eq!(all["solids"][0]["materials"], json!(["CONCRETE/FLOOR"]));

        let bricks = json_of(&run(&mut editor, "list_solids", &json!({ "material": "brick/wall" })));
        assert_eq!(bricks["solids"][0]["solid"], wall);

        run(&mut editor, "select", &json!({ "solids": [wall] }));
        let detail = json_of(&run(&mut editor, "tie_to_entity", &json!({})))["entity"]
            .as_u64()
            .unwrap();
        let tied = json_of(&run(&mut editor, "list_solids", &json!({ "entity": detail })));
        assert_eq!(tied["total"], 1);
        assert_eq!(tied["solids"][0]["entity"], detail);
        let selected = json_of(&run(&mut editor, "list_solids", &json!({ "selected_only": true })));
        assert_eq!(selected["solids"][0]["solid"], wall);
    }

    #[test]
    fn several_keys_are_set_on_several_entities_in_one_step() {
        let (mut editor, _, [near, far, _]) = editor();
        let steps = editor.document.as_ref().unwrap().history.undo_steps();
        let args = json!({ "ids": [near, far], "keys": { "skin": "2", "model": null, "angles": "0 0 0" } });
        let changed = json_of(&run(&mut editor, "set_entity_keys", &args));
        assert_eq!(changed["entities"], 2);
        assert_eq!(changed["keyvalues_changed"], 4, "the angles were zero already");
        let entity = editor.entity(far).unwrap();
        assert_eq!((entity.get("skin"), entity.get("model")), (Some("2"), None));
        assert_eq!(editor.document.as_ref().unwrap().history.undo_steps(), steps + 1);

        run(&mut editor, "select", &json!({ "entities": [near] }));
        let selected = json_of(&run(
            &mut editor,
            "set_entity_keys",
            &json!({ "keys": { "skin": "3" } }),
        ));
        assert_eq!(selected["entities"], 1);
        assert_eq!(editor.entity(far).unwrap().get("skin"), Some("2"));

        run(&mut editor, "clear_selection", &json!({}));
        let nothing = run(&mut editor, "set_entity_keys", &json!({ "keys": { "skin": "3" } }));
        assert!(error_of(&nothing).contains("select entities first"));
        let unknown = run(
            &mut editor,
            "set_entity_keys",
            &json!({ "ids": [9999], "keys": { "skin": "3" } }),
        );
        assert!(error_of(&unknown).contains("no entity has id 9999"));
    }

    #[test]
    fn lines_are_traced_to_the_first_solid_brush_face() {
        let (mut editor, [floor, wall], _) = editor();
        editor.material = "tools/toolstrigger".to_owned();
        editor
            .create_block(DVec3::new(-64.0, -64.0, 32.0), DVec3::new(64.0, 64.0, 64.0))
            .unwrap();
        let rays = json!({ "rays": [
            { "from": [0, 0, 512], "direction": [0, 0, -1] },
            { "from": [0, 0, 64], "to": [512, 0, 64] },
            { "from": [0, 0, 64], "to": [128, 0, 64] },
            { "from": [0, 0, 64], "direction": [0, 0, 1], "distance": 4096 }
        ] });
        let traced = json_of(&run(&mut editor, "trace_line", &rays));
        let hits = traced["hits"].as_array().unwrap();
        assert_eq!(
            hits[0]["point"],
            json!([0.0, 0.0, 0.0]),
            "the trigger brush is passed through"
        );
        assert_eq!(hits[0]["distance"], 512.0);
        assert_eq!(hits[0]["normal"], json!([0.0, 0.0, 1.0]));
        assert_eq!(hits[0]["solid"], floor);
        assert_eq!(hits[0]["material"], "CONCRETE/FLOOR");
        assert_eq!(hits[1]["solid"], wall);
        assert_eq!(hits[1]["point"], json!([240.0, 0.0, 64.0]));
        assert!(hits[2].is_null(), "the wall lies beyond the line's end");
        assert!(hits[3].is_null(), "nothing is above");
        let wrong = run(&mut editor, "trace_line", &json!({ "rays": [{ "from": [0, 0, 0] }] }));
        assert!(error_of(&wrong).contains("ray 0: give either"));
    }
}
