//! `func_instance`: another map placed inside this one, as VBSP merges it in when the
//! map compiles.
//!
//! An instance names its map with `file`, relative to the folder of the map that holds
//! it, and places that map's contents at its `origin`, turned by its `angles`. The names
//! inside get the instance's name before or after them, as its `fixup_style` says, and
//! each `replaceNN` key ("$variable value") swaps a variable for a value in every key.

use std::path::{Path, PathBuf};

use glam::{DMat3, DMat4, DVec3};

use crate::kv::Pair;
use crate::light::vrad_direction;
use crate::transform::{angles_matrix, clean_vec3, linear_part, transform_angles, TextureLock};
use crate::{format_vec3, parse_vec3, Entity, EntityItem, TopLevel, Vmf};

/// The class of an entity that places another map.
pub const INSTANCE_CLASS: &str = "func_instance";

/// How deep instances may sit inside instances before the rest are left out, which also
/// stops an instance that places itself.
pub const MAX_DEPTH: usize = 8;

/// How many folders above a map's own the search for an instance's file climbs.
const SEARCH_LEVELS: usize = 4;

/// Entities that only steer how an instance is placed, which VBSP leaves out of the map.
const PLACEMENT_CLASSES: [&str; 3] = ["func_instance_parms", "func_instance_origin", "func_instance_io_proxy"];

/// Keys that hold an entity name, which get the instance's name added.
const NAME_KEYS: [&str; 6] = [
    "targetname",
    "parentname",
    "target",
    "filtername",
    "damagefilter",
    "lightingorigin",
];

/// Light classes VRAD aims by `angles` with its own `pitch` and `angle` keys.
const AIMED_LIGHTS: [&str; 3] = ["light_spot", "light_environment", "light_directional"];

/// Whether `entity` places another map.
pub fn is_instance(entity: &Entity) -> bool {
    entity.classname().eq_ignore_ascii_case(INSTANCE_CLASS)
}

/// The map an instance names, or `None` when its `file` key is missing or blank.
pub fn instance_file(entity: &Entity) -> Option<&str> {
    entity.get("file").map(str::trim).filter(|file| !file.is_empty())
}

/// Where an instance's `file` may be, in the order to try: in each of `folders`, the
/// folder of the map that holds the instance first, then in the folders above them and
/// in an `instances` folder beside each, as the compilers search. A file without an
/// extension is also looked for as a `.vmf`.
pub fn candidate_paths(file: &str, folders: &[&Path]) -> Vec<PathBuf> {
    let file = file.trim().replace('\\', "/");
    let file = file.trim_start_matches("./");
    let names: Vec<String> = if Path::new(file).extension().is_some() {
        vec![file.to_owned()]
    } else {
        vec![file.to_owned(), format!("{file}.vmf")]
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };
    for name in &names {
        if Path::new(name).is_absolute() {
            push(PathBuf::from(name));
        }
    }
    let relative: Vec<PathBuf> = names
        .iter()
        .filter(|name| !Path::new(name.as_str()).is_absolute())
        .map(|name| name.split('/').filter(|part| !part.is_empty() && *part != ".").collect())
        .collect();
    for folder in folders {
        for name in &relative {
            push(folder.join(name));
        }
    }
    for folder in folders {
        for above in folder.ancestors().take(SEARCH_LEVELS + 1) {
            for name in &relative {
                push(above.join(name));
                push(above.join("instances").join(name));
            }
        }
    }
    paths
}

/// How an instance's names change, from its `fixup_style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixupStyle {
    /// `name-inner`, the default.
    Prefix,
    /// `inner-name`.
    Postfix,
    /// Names stay as they are.
    None,
}

/// Where an instance puts its map's contents and how it renames them.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// The instance's map to the map that holds it: `origin`, then `angles`.
    pub matrix: DMat4,
    /// The instance's own name, which names inside it get; `None` when it has none.
    pub name: Option<String>,
    pub style: FixupStyle,
    /// Each `$variable` and what it stands for, longest variable first.
    pub replacements: Vec<(String, String)>,
}

impl Placement {
    /// Reads where an instance entity places its map.
    pub fn of(entity: &Entity) -> Self {
        let mut replacements: Vec<(String, String)> = entity
            .items
            .iter()
            .filter_map(|item| match item {
                EntityItem::Key(pair) if is_replacement_key(&pair.key) => {
                    let text = pair.value.trim();
                    let (variable, value) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
                    (variable.len() > 1 && variable.starts_with('$'))
                        .then(|| (variable.to_owned(), value.trim().to_owned()))
                }
                _ => None,
            })
            .collect();
        replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        let style = match entity.get("fixup_style").map(str::trim) {
            Some("1") => FixupStyle::Postfix,
            Some("2") => FixupStyle::None,
            _ => FixupStyle::Prefix,
        };
        Self {
            matrix: instance_matrix(entity),
            name: entity
                .get("targetname")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            style,
            replacements,
        }
    }

    /// `inner` as it is named inside the map that holds the instance. Names that start
    /// with `@` or `!` are left alone, as VBSP leaves them.
    pub fn fix_name(&self, inner: &str) -> String {
        let Some(name) = self.name.as_deref() else {
            return inner.to_owned();
        };
        if inner.is_empty() || inner.starts_with('@') || inner.starts_with('!') {
            return inner.to_owned();
        }
        match self.style {
            FixupStyle::Prefix => format!("{name}-{inner}"),
            FixupStyle::Postfix => format!("{inner}-{name}"),
            FixupStyle::None => inner.to_owned(),
        }
    }

    /// `value` with every `$variable` swapped for what it stands for.
    pub fn replace(&self, value: &str) -> String {
        let mut value = value.to_owned();
        for (variable, replacement) in &self.replacements {
            if value.contains(variable.as_str()) {
                value = value.replace(variable.as_str(), replacement);
            }
        }
        value
    }
}

/// Whether `key` is one of an instance's `replaceNN` keys.
fn is_replacement_key(key: &str) -> bool {
    key.len() > "replace".len()
        && key[.."replace".len()].eq_ignore_ascii_case("replace")
        && key["replace".len()..].bytes().all(|byte| byte.is_ascii_digit())
}

/// The instance's map to the map that holds it: turned by `angles` about the instance's
/// own origin, then moved to `origin`.
pub fn instance_matrix(entity: &Entity) -> DMat4 {
    let origin = entity.origin().unwrap_or(DVec3::ZERO);
    let angles = entity.get("angles").and_then(parse_vec3).unwrap_or(DVec3::ZERO);
    DMat4::from_translation(origin) * DMat4::from_mat3(angles_matrix(angles))
}

/// An instance's map, placed: its world brushes and entities carried into the map that
/// holds the instance, renamed and with its variables replaced. The entities that only
/// steer placement are left out, and a `func_instance_origin` in the map is where its
/// contents are placed from. Instances inside it are placed with the rest, so they keep
/// their place relative to it, but their maps are not read here.
pub fn place(contents: &Vmf, placement: &Placement) -> Vmf {
    let pivot = contents
        .entities()
        .find(|entity| entity.classname().eq_ignore_ascii_case("func_instance_origin"))
        .and_then(Entity::origin)
        .unwrap_or(DVec3::ZERO);
    let matrix = placement.matrix * DMat4::from_translation(-pivot);
    let items = contents
        .items
        .iter()
        .filter_map(|item| match item {
            TopLevel::World(world) => {
                let mut world = world.clone();
                for solid in world.solids_mut() {
                    solid.apply_matrix(&matrix, TextureLock::Full);
                }
                Some(TopLevel::World(world))
            }
            TopLevel::Entity(entity) if !is_placement_entity(entity) => {
                let mut entity = entity.clone();
                fix_keys(&mut entity, placement);
                transform_entity(&mut entity, &matrix);
                Some(TopLevel::Entity(entity))
            }
            _ => None,
        })
        .collect();
    Vmf::with_items(items)
}

fn is_placement_entity(entity: &Entity) -> bool {
    PLACEMENT_CLASSES
        .iter()
        .any(|class| entity.classname().eq_ignore_ascii_case(class))
}

/// Replaces the instance's variables in every key of `entity` and gives its names the
/// instance's name.
fn fix_keys(entity: &mut Entity, placement: &Placement) {
    for item in &mut entity.items {
        let EntityItem::Key(pair) = item else {
            continue;
        };
        if !placement.replacements.is_empty() {
            pair.value = placement.replace(&pair.value);
        }
        if NAME_KEYS.iter().any(|key| pair.key.eq_ignore_ascii_case(key)) {
            *pair = Pair::new(pair.key.clone(), placement.fix_name(pair.value.trim()));
        }
    }
}

/// Carries an entity of an instance's map into the map that holds it: its brushes with
/// their textures, its origin, an overlay's plane and corners, and which way it faces.
fn transform_entity(entity: &mut Entity, matrix: &DMat4) {
    let point = entity.solids().next().is_none();
    for solid in entity.solids_mut() {
        solid.apply_matrix(matrix, TextureLock::Full);
    }
    if let Some(origin) = entity.origin() {
        entity.set("origin", format_vec3(clean_vec3(matrix.transform_point3(origin))));
    }
    crate::overlay::transform_entity(entity, matrix);
    let linear = linear_part(matrix);
    if linear.abs_diff_eq(DMat3::IDENTITY, 1e-12) {
        return;
    }
    if AIMED_LIGHTS
        .iter()
        .any(|class| entity.classname().eq_ignore_ascii_case(class))
    {
        aim_light(entity, linear);
        return;
    }
    let angles = entity.get("angles").and_then(parse_vec3);
    let Some(start) = angles.or(point.then_some(DVec3::ZERO)) else {
        return;
    };
    if let Some(turned) = transform_angles(start, linear) {
        entity.set("angles", format_vec3(turned));
    }
}

/// Turns a light VRAD aims by `angles`, `pitch` and `angle` so it shines the turned way.
/// VRAD's pitch points up where the engine's points down, and its `pitch` and `angle`
/// keys stand in for the pitch and yaw of `angles` when they are set.
fn aim_light(entity: &mut Entity, linear: DMat3) {
    let direction = vrad_direction(entity);
    let turned = (linear * direction).normalize_or(direction);
    let roll = entity.get("angles").and_then(parse_vec3).map_or(0.0, |angles| angles.z);
    let pitch = clean_degrees(turned.z.clamp(-1.0, 1.0).asin().to_degrees());
    let flat = turned.x.hypot(turned.y) > 1e-9;
    let yaw = if flat {
        clean_degrees(turned.y.atan2(turned.x).to_degrees().rem_euclid(360.0))
    } else {
        0.0
    };
    entity.set("angles", format_vec3(DVec3::new(pitch, yaw, roll)));
    if entity.get("pitch").is_some() {
        entity.set("pitch", format_number(pitch));
    }
    if entity.get("angle").is_some() {
        let angle = match (flat, turned.z > 0.0) {
            (true, _) => yaw,
            (false, true) => -1.0,
            (false, false) => -2.0,
        };
        entity.set("angle", format_number(angle));
    }
}

/// Degrees rounded to a thousandth, without a negative zero or a full turn.
fn clean_degrees(degrees: f64) -> f64 {
    let rounded = (degrees * 1000.0).round() / 1000.0 + 0.0;
    if rounded >= 360.0 {
        rounded - 360.0
    } else {
        rounded
    }
}

fn format_number(value: f64) -> String {
    format_vec3(DVec3::splat(value))
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(keys: &[(&str, &str)]) -> Entity {
        let mut entity = Entity::default();
        for (key, value) in keys {
            entity.set(key, *value);
        }
        entity
    }

    const BOX: &str = r#"world
{
	"id" "1"
	"classname" "worldspawn"
	solid
	{
		"id" "2"
		side
		{
			"id" "1"
			"plane" "(0 0 16) (16 0 16) (16 -16 16)"
			"material" "DEV/DEV_MEASUREGENERIC01B"
			"uaxis" "[1 0 0 0] 0.25"
			"vaxis" "[0 -1 0 0] 0.25"
			"rotation" "0"
			"lightmapscale" "16"
			"smoothing_groups" "0"
		}
	}
}
entity
{
	"id" "3"
	"classname" "light_spot"
	"origin" "32 0 8"
	"angles" "0 0 0"
	"pitch" "-90"
	"targetname" "lamp"
}
entity
{
	"id" "4"
	"classname" "prop_static"
	"origin" "16 0 0"
	"angles" "0 90 0"
	"model" "$prop"
}
entity
{
	"id" "5"
	"classname" "func_instance_origin"
	"origin" "0 0 0"
}
"#;

    #[test]
    fn looks_beside_the_map_then_above_it() {
        let folder = Path::new("C:/maps/mapsrc");
        let paths = candidate_paths("instances\\road_kit\\corner.vmf", &[folder]);
        assert_eq!(paths[0], folder.join("instances/road_kit/corner.vmf"));
        assert!(paths.contains(&folder.join("instances").join("instances/road_kit/corner.vmf")));
        let nested = Path::new("C:/maps/mapsrc/instances/road_kit");
        let paths = candidate_paths("instances/road_kit/tee.vmf", &[nested, folder]);
        assert_eq!(paths[0], nested.join("instances/road_kit/tee.vmf"));
        assert_eq!(paths[1], folder.join("instances/road_kit/tee.vmf"));
        assert!(paths.contains(&Path::new("C:/maps/mapsrc").join("instances/road_kit/tee.vmf")));
    }

    #[test]
    fn adds_the_extension_when_missing() {
        let folder = Path::new("maps");
        let paths = candidate_paths("parts/door", &[folder]);
        assert_eq!(paths[0], folder.join("parts/door"));
        assert_eq!(paths[1], folder.join("parts/door.vmf"));
    }

    #[test]
    fn keeps_absolute_paths_first() {
        let absolute = if cfg!(windows) { "C:/kit/a.vmf" } else { "/kit/a.vmf" };
        let paths = candidate_paths(absolute, &[Path::new("maps")]);
        assert_eq!(paths, vec![PathBuf::from(absolute)]);
    }

    #[test]
    fn reads_the_placement() {
        let instance = entity(&[
            ("classname", "func_instance"),
            ("origin", "100 200 300"),
            ("angles", "0 90 0"),
            ("targetname", "road"),
            ("replace01", "$prop models/a.mdl"),
            ("replace02", "$p models/b.mdl"),
        ]);
        let placement = Placement::of(&instance);
        let moved = placement.matrix.transform_point3(DVec3::new(10.0, 0.0, 0.0));
        assert!(moved.abs_diff_eq(DVec3::new(100.0, 210.0, 300.0), 1e-9));
        assert_eq!(placement.style, FixupStyle::Prefix);
        assert_eq!(placement.fix_name("lamp"), "road-lamp");
        assert_eq!(placement.fix_name("@global"), "@global");
        assert_eq!(placement.replace("$prop"), "models/a.mdl");
        assert_eq!(placement.replace("$p"), "models/b.mdl");
    }

    #[test]
    fn places_brushes_entities_and_lights() {
        let contents = Vmf::parse(BOX).unwrap();
        let instance = entity(&[
            ("classname", "func_instance"),
            ("origin", "1000 0 64"),
            ("angles", "0 90 0"),
            ("targetname", "road"),
            ("fixup_style", "1"),
            ("replace01", "$prop models/props_c17/bench01a.mdl"),
        ]);
        let placed = place(&contents, &Placement::of(&instance));
        let side = placed.world().unwrap().solids().next().unwrap().sides().next().unwrap();
        assert!(side.plane[1].abs_diff_eq(DVec3::new(1000.0, 16.0, 80.0), 1e-9));
        assert!(side.uaxis.axis.abs_diff_eq(DVec3::Y, 1e-9));
        let entities: Vec<&Entity> = placed.entities().collect();
        assert_eq!(entities.len(), 2, "the placement entity is left out");
        let light = entities[0];
        assert_eq!(light.origin(), Some(DVec3::new(1000.0, 32.0, 72.0)));
        assert_eq!(light.get("targetname"), Some("lamp-road"));
        assert!(vrad_direction(light).abs_diff_eq(DVec3::NEG_Z, 1e-6));
        let prop = entities[1];
        assert_eq!(prop.origin(), Some(DVec3::new(1000.0, 16.0, 64.0)));
        assert_eq!(prop.get("angles"), Some("0 180 0"));
        assert_eq!(prop.get("model"), Some("models/props_c17/bench01a.mdl"));
    }

    #[test]
    fn turns_aimed_lights_with_the_instance() {
        let mut spot = entity(&[("classname", "light_spot"), ("origin", "0 0 0"), ("angles", "-30 0 0")]);
        transform_entity(&mut spot, &DMat4::from_mat3(angles_matrix(DVec3::new(0.0, 90.0, 0.0))));
        let expected = DVec3::new(0.0, 30f64.to_radians().cos(), -(30f64.to_radians().sin()));
        assert!(vrad_direction(&spot).abs_diff_eq(expected, 1e-6));
    }

    #[test]
    fn places_from_the_instance_origin_entity() {
        let mut contents = Vmf::parse(BOX).unwrap();
        for item in &mut contents.items {
            if let TopLevel::Entity(entity) = item {
                if entity.classname() == "func_instance_origin" {
                    entity.set("origin", "16 0 0");
                }
            }
        }
        let instance = entity(&[("classname", "func_instance"), ("origin", "0 0 0")]);
        let placed = place(&contents, &Placement::of(&instance));
        let prop = placed.entities().nth(1).unwrap();
        assert_eq!(prop.origin(), Some(DVec3::ZERO));
    }
}
