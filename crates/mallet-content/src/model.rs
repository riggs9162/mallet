//! Studio models (`.mdl` with its `.vvd` and `.vtx`), as placed by prop entities.

use mallet_kv::{Block, Item};
use vmdl::{Mdl, Vtx, Vvd};

use crate::vpk::normalize;
use crate::GameFs;

/// One vertex of a model, in the model's own space (X forward, Y left, Z up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

/// The triangles of a model that share one material.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelMesh {
    /// Material path under `materials/`, without extension, such as
    /// `models/props_c17/oildrum001`.
    pub material: String,
    /// Triangle list indices into [`Model::vertices`].
    pub indices: Vec<u32>,
}

/// A model's highest-detail geometry with its default skin.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub vertices: Vec<ModelVertex>,
    pub meshes: Vec<ModelMesh>,
    /// Smallest corner of the model's box: the hull the model was compiled with, as
    /// Hammer and the game use, or the geometry's bounds when it has none.
    pub min: [f32; 3],
    /// Largest corner of the model's box.
    pub max: [f32; 3],
}

impl Model {
    /// Number of triangles in all meshes.
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|mesh| mesh.indices.len() / 3).sum()
    }
}

/// Which look of a model to load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ModelVariant {
    /// The skin family, as an entity's `skin` key numbers them. One the model lacks
    /// loads the first.
    pub skin: usize,
    /// The body groups to show, as an entity's `body` key packs them. `None` loads the
    /// sub-models of every group at once.
    pub body: Option<u32>,
}

/// One body group of a model: a part that can be swapped between sub-models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyGroup {
    /// The sub-models the group chooses between, by the name they were compiled with. A
    /// sub-model without meshes shows nothing, as the `blank` choice of a group does.
    pub choices: Vec<String>,
}

/// The prop data a model was compiled with: the `prop_data` section of its `$keyvalues`,
/// which makes it a physics prop to the game and the compilers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PropData {
    /// The entry of the game's `scripts/propdata.txt` it builds on, such as `Wooden.Large`.
    pub base: Option<String>,
    /// `allowstatic`: the model may be a static prop all the same.
    pub allow_static: bool,
}

/// What decides which prop classes take a model: whether it was compiled as a static
/// prop, and its prop data. Both are read from the start of a model's file and its
/// keyvalues, without the rest of the model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PropTraits {
    /// Compiled with `$staticprop`, as `prop_static` needs.
    pub static_prop: bool,
    /// The model's prop data, when it has any.
    pub prop_data: Option<PropData>,
}

/// The prop classes a map usually places a model with, in the order a class is picked
/// for a model: a static prop where the compiler takes it, then a physics prop, then a
/// dynamic one.
pub const PROP_CLASSES: [&str; 3] = ["prop_static", "prop_physics", "prop_dynamic"];

impl PropTraits {
    /// Why entities of a prop class lose a model with these traits, by the rules of the
    /// Source SDK: VBSP's `IsStaticProp` for `prop_static`, and the game's
    /// `CBaseProp::Spawn` for the classes it spawns. `None` when the class takes the
    /// model. `collision` says whether the model has a collision model.
    pub fn class_problem(&self, classname: &str, collision: bool) -> Option<&'static str> {
        let classname = classname.trim().to_ascii_lowercase();
        let physics_data = self.prop_data.as_ref().is_some_and(|data| !data.allow_static);
        match classname.as_str() {
            "prop_static" if !self.static_prop => {
                Some("It was not compiled with $staticprop, so VBSP leaves it out of the map.")
            }
            "prop_static" if physics_data => {
                Some("It has prop data, which makes it a physics prop, so VBSP leaves it out of the map.")
            }
            "prop_dynamic" | "prop_dynamic_ornament" if physics_data => Some(
                "It has prop data, which makes it a physics prop, so the game removes it; prop_dynamic_override takes it.",
            ),
            name if (name.starts_with("prop_physics") || name == "prop_ragdoll") && !collision => {
                Some("It has no collision model, so the game removes it as a physics prop.")
            }
            "prop_physics" if self.prop_data.is_none() => {
                Some("It has no prop data, so the game removes it; prop_physics_override takes it.")
            }
            _ => None,
        }
    }

    /// Whether entities of a prop class keep a model with these traits.
    pub fn works_as(&self, classname: &str, collision: bool) -> bool {
        self.class_problem(classname, collision).is_none()
    }

    /// The class to place a model with these traits as: the first of [`PROP_CLASSES`]
    /// that takes it, or `prop_dynamic_override`, which takes any model.
    pub fn default_class(&self, collision: bool) -> &'static str {
        PROP_CLASSES
            .into_iter()
            .find(|class| self.works_as(class, collision))
            .unwrap_or("prop_dynamic_override")
    }

    /// The class to turn a prop into when its own class loses a model with these traits:
    /// one that keeps the model and leaves the prop doing what it did. A static prop
    /// becomes a dynamic one, which stays where it is put all the same, and a dynamic or
    /// a physics prop becomes the `_override` class that stands in for it. `None` when
    /// the class keeps the model, and when no class stands in for it, as for a physics
    /// prop without a collision model.
    pub fn replacement_class(&self, classname: &str, collision: bool) -> Option<&'static str> {
        self.class_problem(classname, collision)?;
        let replacement = match classname.trim().to_ascii_lowercase().as_str() {
            "prop_static" if self.works_as("prop_dynamic", collision) => "prop_dynamic",
            "prop_static" | "prop_dynamic" => "prop_dynamic_override",
            "prop_physics" => "prop_physics_override",
            _ => return None,
        };
        self.works_as(replacement, collision).then_some(replacement)
    }
}

/// What a model is, beyond its geometry, for the model browser.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelDetails {
    /// How many skin families the model has; at least one.
    pub skins: usize,
    pub body_groups: Vec<BodyGroup>,
    /// Compiled with `$staticprop`, as `prop_static` needs.
    pub static_prop: bool,
    /// The model's prop data, when it has any.
    pub prop_data: Option<PropData>,
    /// The surface property the model was compiled with, such as `metal`.
    pub surface_prop: String,
    pub bones: usize,
    /// The names of the model's animation sequences.
    pub sequences: Vec<String>,
    pub vertices: usize,
}

impl ModelDetails {
    /// What decides which prop classes take this model.
    pub fn prop_traits(&self) -> PropTraits {
        PropTraits {
            static_prop: self.static_prop,
            prop_data: self.prop_data.clone(),
        }
    }

    /// Why entities of a prop class lose this model, as [`PropTraits::class_problem`]
    /// tells it; `None` when the class takes the model.
    pub fn prop_class_problem(&self, classname: &str, collision: bool) -> Option<&'static str> {
        self.prop_traits().class_problem(classname, collision)
    }

    /// The `body` value that shows the given choice of each body group.
    pub fn body_value(&self, choices: &[usize]) -> u32 {
        let mut value = 0_u32;
        let mut base = 1_u32;
        for (group, choice) in self.body_groups.iter().zip(choices) {
            let count = group.choices.len().max(1) as u32;
            value = value.saturating_add(base.saturating_mul((*choice as u32).min(count - 1)));
            base = base.saturating_mul(count);
        }
        value
    }
}

/// The choice a `body` value makes in each body group with these many sub-models.
fn body_choices(counts: &[usize], body: u32) -> Vec<usize> {
    let mut base = 1_u32;
    counts
        .iter()
        .map(|count| {
            let count = (*count).max(1) as u32;
            let choice = (body / base.max(1)) % count;
            base = base.saturating_mul(count);
            choice as usize
        })
        .collect()
}

/// The four bytes every compiled model's `.mdl` file starts with.
const MODEL_MAGIC: &[u8; 4] = b"IDST";
/// How much of a `.mdl` file's start holds everything [`prop_traits`] reads from it.
const TRAITS_HEADER_BYTES: usize = 320;
/// Where the flags of a compiled model lie in its `.mdl` file.
const FLAGS_OFFSET: usize = 152;
/// Where a `.mdl` file says where its keyvalues start, and after it how long they are.
const KEY_VALUES_OFFSET: usize = 312;
/// The flag `$staticprop` sets.
const STATIC_PROP_FLAG: u32 = 0x10;
/// The longest keyvalues [`prop_traits`] reads. Real ones are a few hundred bytes.
const MAX_KEY_VALUES_BYTES: u64 = 1024 * 1024;

/// What decides which prop classes take a model, read from the start of its `.mdl` and
/// its keyvalues alone: two short reads, where loading the model reads all of it and
/// its vertex files. `None` when the file is missing, cut short or not a model.
pub fn prop_traits(filesystem: &GameFs, path: &str) -> Option<PropTraits> {
    let file = filesystem.open(&normalize(path))?;
    let header = file.read(0, TRAITS_HEADER_BYTES as u64)?;
    if header.len() < TRAITS_HEADER_BYTES || !header.starts_with(MODEL_MAGIC) {
        return None;
    }
    let number = |at: usize| i32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    let (start, length) = (number(KEY_VALUES_OFFSET), number(KEY_VALUES_OFFSET + 4));
    let key_values = (start > 0 && length > 0)
        .then(|| file.read(start as u64, (length as u64).min(MAX_KEY_VALUES_BYTES)))
        .flatten()
        .unwrap_or_default();
    let text = key_values.split(|byte| *byte == 0).next().unwrap_or_default();
    Some(PropTraits {
        static_prop: number(FLAGS_OFFSET) as u32 & STATIC_PROP_FLAG != 0,
        prop_data: prop_data(&String::from_utf8_lossy(text)),
    })
}

/// Loads a model such as `models/props_c17/oildrum001.mdl` from the game's files.
///
/// Uses the first level of detail, the default skin and the model's idle orientation.
/// Each mesh's material is looked up in the model's material folders in order, like the
/// engine does.
pub fn load_model(filesystem: &GameFs, path: &str) -> Result<Model, String> {
    load_model_variant(filesystem, path, ModelVariant::default()).map(|(model, _)| model)
}

/// Loads a model like [`load_model`], with the chosen skin and body groups, and says
/// what the model is.
pub fn load_model_variant(
    filesystem: &GameFs,
    path: &str,
    variant: ModelVariant,
) -> Result<(Model, ModelDetails), String> {
    // The parser panics on some model features it does not support yet, such as
    // animations stored in a separate block, so a panic counts as a failed load.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_model(filesystem, path, variant)))
        .unwrap_or_else(|_| Err(format!("{path} uses features the model parser does not support")))
}

fn read_model(filesystem: &GameFs, path: &str, variant: ModelVariant) -> Result<(Model, ModelDetails), String> {
    let path = normalize(path);
    let base = path.strip_suffix(".mdl").unwrap_or(&path);
    let read = |suffix: &str| {
        filesystem
            .read(&format!("{base}{suffix}"))
            .ok_or_else(|| format!("{base}{suffix} not found"))
    };
    let mdl = Mdl::read(&read(".mdl")?).map_err(|error| error.to_string())?;
    let vtx_data = read(".dx90.vtx").or_else(|_| read(".vtx"))?;
    let vtx = Vtx::read(&vtx_data).map_err(|error| error.to_string())?;
    let vvd = Vvd::read(&read(".vvd")?).map_err(|error| error.to_string())?;
    let body_groups: Vec<BodyGroup> = mdl
        .body_parts
        .iter()
        .map(|part| BodyGroup {
            choices: part.models.iter().map(|model| model.name.as_str().to_owned()).collect(),
        })
        .collect();
    let mesh_owners: Vec<(usize, usize)> = mdl
        .body_parts
        .iter()
        .enumerate()
        .flat_map(|(group, part)| {
            part.models
                .iter()
                .enumerate()
                .flat_map(move |(choice, model)| model.meshes.iter().map(move |_| (group, choice)))
        })
        .collect();
    let group_sizes: Vec<usize> = body_groups.iter().map(|group| group.choices.len()).collect();
    let shown = variant.body.map(|body| body_choices(&group_sizes, body));
    let mut details = ModelDetails {
        skins: 1,
        body_groups,
        static_prop: mdl.header.flags.contains(vmdl::mdl::ModelFlags::STATIC_PROP),
        prop_data: mdl.key_values.as_deref().and_then(prop_data),
        surface_prop: mdl.surface_prop.clone(),
        bones: mdl.bones.len(),
        sequences: mdl
            .animation_sequences
            .iter()
            .map(|sequence| sequence.label.clone())
            .collect(),
        vertices: 0,
    };
    let model = vmdl::Model::from_parts(mdl, vtx, vvd);
    details.skins = model.skin_tables().count().max(1);
    let (hull_min, hull_max) = model.bounding_box();

    let basis = root_basis(&model);
    let vertices: Vec<ModelVertex> = model
        .vertices()
        .iter()
        .map(|vertex| ModelVertex {
            position: turn(&basis, vertex.position),
            normal: turn(&basis, vertex.normal),
            uv: vertex.texture_coordinates,
        })
        .collect();

    let directories: Vec<String> = model
        .texture_directories()
        .iter()
        .map(|directory| normalize(directory).trim_end_matches('/').to_owned())
        .collect();
    let skin = model
        .skin_tables()
        .nth(variant.skin)
        .or_else(|| model.skin_tables().next());
    let mut meshes = Vec::new();
    for (number, mesh) in model.meshes().enumerate() {
        let hidden = shown.as_ref().is_some_and(|shown| {
            mesh_owners
                .get(number)
                .is_some_and(|(group, choice)| shown.get(*group) != Some(choice))
        });
        if hidden {
            continue;
        }
        let index = mesh.material_index();
        let texture = skin
            .as_ref()
            .and_then(|skin| skin.texture(index))
            .or_else(|| model.textures().get(index as usize).map(|info| info.name.as_str()))
            .unwrap_or_default();
        let indices: Vec<u32> = mesh
            .vertex_strip_indices()
            .flatten()
            .filter(|index| *index < vertices.len())
            .map(|index| index as u32)
            .collect();
        if indices.len() < 3 {
            continue;
        }
        meshes.push(ModelMesh {
            material: resolve_material(filesystem, &directories, texture),
            indices,
        });
    }

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    if vertices.is_empty() {
        return Err(format!("{base}.mdl has no vertices"));
    }
    let (hull_min, hull_max) = (
        [hull_min.x, hull_min.y, hull_min.z],
        [hull_max.x, hull_max.y, hull_max.z],
    );
    if (0..3).all(|axis| hull_max[axis] > hull_min[axis]) {
        (min, max) = (hull_min, hull_max);
    }
    details.vertices = vertices.len();
    Ok((
        Model {
            vertices,
            meshes,
            min,
            max,
        },
        details,
    ))
}

/// The `prop_data` section of a model's keyvalues, which lies at their top or inside
/// the block that wraps them. Text that cannot be read as keyvalues counts as having
/// the section when it names it.
fn prop_data(key_values: &str) -> Option<PropData> {
    let named = |block: &&Block| block.name.eq_ignore_ascii_case("prop_data");
    let Ok(items) = mallet_kv::parse(key_values) else {
        let text = key_values.to_ascii_lowercase();
        return text.contains("prop_data").then(|| PropData {
            base: None,
            allow_static: text.contains("allowstatic"),
        });
    };
    let top: Vec<&Block> = items
        .iter()
        .filter_map(|item| match item {
            Item::Block(block) => Some(block),
            Item::Pair(_) => None,
        })
        .collect();
    let section = top
        .iter()
        .copied()
        .find(|block| named(block))
        .or_else(|| top.iter().flat_map(|block| block.blocks()).find(named))?;
    let set = |key: &str| {
        section
            .get(key)
            .and_then(|value| value.trim().parse::<f32>().ok())
            .is_some_and(|value| value != 0.0)
    };
    Some(PropData {
        base: section
            .get("base")
            .map(|base| base.trim().to_owned())
            .filter(|base| !base.is_empty()),
        allow_static: set("allowstatic"),
    })
}

/// Where the stored X, Y and Z axes of a model's vertices point in the model's own space.
///
/// A static prop stores its vertices as they are shown. Any other model stores them a
/// quarter turn about the up axis away, and the parser works the turn out from the root
/// bone and the idle animation. For some characters it reads another animation than the
/// idle one and ends up with a model on its head; a turn that does not keep up pointing
/// up is replaced by the quarter turn.
fn root_basis(model: &vmdl::Model) -> [[f32; 3]; 3] {
    let axis = |x: f32, y: f32, z: f32| {
        let turned = model.apply_root_transform(vmdl::Vector { x, y, z });
        [turned.x, turned.y, turned.z]
    };
    let basis = [axis(1.0, 0.0, 0.0), axis(0.0, 1.0, 0.0), axis(0.0, 0.0, 1.0)];
    let [across, along, up] = basis[2];
    if across.abs() < 0.01 && along.abs() < 0.01 && (up - 1.0).abs() < 0.01 {
        basis
    } else {
        [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]
    }
}

/// Turns a stored position or normal into the model's own space.
fn turn(basis: &[[f32; 3]; 3], vector: vmdl::Vector) -> [f32; 3] {
    [0, 1, 2].map(|axis| vector.x * basis[0][axis] + vector.y * basis[1][axis] + vector.z * basis[2][axis])
}

/// The first `directory/texture` that has a material file, or the first candidate when
/// none does, so the renderer shows its missing-texture pattern.
fn resolve_material(filesystem: &GameFs, directories: &[String], texture: &str) -> String {
    let texture = normalize(texture);
    let candidates: Vec<String> = if directories.is_empty() {
        vec![texture.clone()]
    } else {
        directories
            .iter()
            .map(|directory| {
                if directory.is_empty() {
                    texture.clone()
                } else {
                    format!("{directory}/{texture}")
                }
            })
            .collect()
    };
    candidates
        .iter()
        .find(|candidate| filesystem.exists(&format!("materials/{candidate}.vmt")))
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(static_prop: bool, prop_data: Option<PropData>) -> ModelDetails {
        ModelDetails {
            static_prop,
            prop_data,
            ..ModelDetails::default()
        }
    }

    #[test]
    fn prop_data_is_found_in_a_models_keyvalues() {
        let wrapped = "mdlkeyvalue\n{\n\tprop_data\n\t{\n\t\t\"base\" \"Wooden.Large\"\n\t}\n}\n";
        assert_eq!(
            prop_data(wrapped),
            Some(PropData {
                base: Some("Wooden.Large".to_owned()),
                allow_static: false,
            })
        );
        let bare = "\"Prop_Data\" { \"base\" \"Metal.Medium\" \"allowstatic\" \"1\" }";
        assert!(prop_data(bare).unwrap().allow_static);
        assert_eq!(prop_data("mdlkeyvalue { \"qc_path\" { \"value\" \"a.qc\" } }"), None);
        assert_eq!(prop_data(""), None);
        assert!(prop_data("prop_data { \"base\" \"unclosed").is_some());
    }

    #[test]
    fn prop_classes_follow_the_rules_of_the_compiler_and_the_game() {
        let physics = Some(PropData {
            base: Some("Wooden.Large".to_owned()),
            allow_static: false,
        });
        let either = Some(PropData {
            base: None,
            allow_static: true,
        });
        let plain = details(true, None);
        assert_eq!(plain.prop_class_problem("prop_static", false), None);
        assert_eq!(plain.prop_class_problem("prop_dynamic", false), None);
        assert!(plain.prop_class_problem("prop_physics", true).unwrap().contains("no prop data"));
        assert_eq!(plain.prop_class_problem("prop_physics_override", true), None);
        let neither = plain.prop_class_problem("prop_physics", false).unwrap();
        assert!(neither.contains("no collision model"), "{neither}");

        let bench = details(true, physics.clone());
        assert!(bench.prop_class_problem("prop_static", true).unwrap().contains("VBSP"));
        assert!(bench.prop_class_problem("PROP_DYNAMIC", true).unwrap().contains("prop_dynamic_override"));
        assert_eq!(bench.prop_class_problem("prop_dynamic_override", true), None);
        assert_eq!(bench.prop_class_problem("prop_physics", true), None);
        assert!(bench.prop_class_problem("prop_physics", false).unwrap().contains("collision"));
        assert!(bench.prop_class_problem("prop_physics_multiplayer", false).is_some());

        let allowed = details(true, either);
        assert_eq!(allowed.prop_class_problem("prop_static", true), None);
        assert_eq!(allowed.prop_class_problem("prop_dynamic", true), None);
        assert_eq!(allowed.prop_class_problem("prop_physics", true), None);

        let animated = details(false, None);
        assert!(animated.prop_class_problem("prop_static", true).unwrap().contains("$staticprop"));
        assert_eq!(animated.prop_class_problem("prop_dynamic", true), None);
        assert_eq!(animated.prop_class_problem("npc_citizen", false), None);
    }

    #[test]
    fn a_model_is_placed_as_the_first_class_that_takes_it() {
        let data = |allow_static: bool| {
            Some(PropData {
                base: Some("Wooden.Large".to_owned()),
                allow_static,
            })
        };
        let traits = |static_prop: bool, prop_data: Option<PropData>| PropTraits { static_prop, prop_data };
        assert_eq!(traits(true, None).default_class(true), "prop_static");
        assert_eq!(traits(true, data(false)).default_class(true), "prop_physics");
        assert_eq!(traits(true, data(false)).default_class(false), "prop_dynamic_override");
        assert_eq!(traits(true, data(true)).default_class(true), "prop_static");
        assert_eq!(traits(false, data(true)).default_class(true), "prop_physics");
        assert_eq!(traits(false, None).default_class(true), "prop_dynamic");
        assert!(traits(true, None).works_as("prop_static", false));
        assert!(!traits(true, data(false)).works_as("prop_static", true));
    }

    #[test]
    fn a_prop_that_loses_its_model_gets_a_class_that_does_the_same() {
        let data = Some(PropData {
            base: Some("Metal.Medium".to_owned()),
            allow_static: false,
        });
        let drum = PropTraits {
            static_prop: true,
            prop_data: data,
        };
        let lamp = PropTraits {
            static_prop: true,
            prop_data: None,
        };
        let citizen = PropTraits::default();
        let (dynamic, physics) = (Some("prop_dynamic_override"), Some("prop_physics_override"));
        assert_eq!(drum.replacement_class("prop_static", true), dynamic);
        assert_eq!(drum.replacement_class("Prop_Dynamic", true), dynamic);
        assert_eq!(drum.replacement_class("prop_physics", true), None, "it keeps the model");
        assert_eq!(drum.replacement_class("prop_physics", false), None, "nothing stands in");
        assert_eq!(drum.replacement_class("prop_dynamic_ornament", true), None);
        assert_eq!(citizen.replacement_class("prop_static", true), Some("prop_dynamic"));
        assert_eq!(lamp.replacement_class("prop_static", false), None);
        assert_eq!(lamp.replacement_class("prop_physics", true), physics);
        assert_eq!(lamp.replacement_class("prop_physics", false), None);
        assert_eq!(lamp.replacement_class("prop_ragdoll", false), None);
    }

    /// The start of a `.mdl` file with these flags, followed by its keyvalues.
    fn model_file(flags: u32, key_values: &str) -> Vec<u8> {
        let mut file = vec![0_u8; TRAITS_HEADER_BYTES];
        file[..4].copy_from_slice(MODEL_MAGIC);
        file[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&flags.to_le_bytes());
        if !key_values.is_empty() {
            let start = (TRAITS_HEADER_BYTES as i32 + 16).to_le_bytes();
            let length = (key_values.len() as i32 + 1).to_le_bytes();
            file[KEY_VALUES_OFFSET..KEY_VALUES_OFFSET + 4].copy_from_slice(&start);
            file[KEY_VALUES_OFFSET + 4..KEY_VALUES_OFFSET + 8].copy_from_slice(&length);
            file.extend([0xAA; 16]);
            file.extend(key_values.as_bytes());
            file.push(0);
            file.extend(b"what follows the keyvalues");
        }
        file
    }

    #[test]
    fn prop_traits_are_read_from_the_start_of_a_model_file() {
        let dir = std::env::temp_dir().join(format!("mallet-prop-traits-{}", std::process::id()));
        let models = dir.join("models").join("test");
        std::fs::create_dir_all(&models).unwrap();
        let gameinfo = "\"GameInfo\"\n{\n\tFileSystem\n\t{\n\t\tSearchPaths\n\t\t{\n\t\t\tgame \"|gameinfo_path|.\"\n\t\t}\n\t}\n}\n";
        std::fs::write(dir.join("gameinfo.txt"), gameinfo).unwrap();
        let bench = "mdlkeyvalue\n{\n\tprop_data\n\t{\n\t\t\"base\" \"Wooden.Large\"\n\t}\n}\n";
        std::fs::write(models.join("bench.mdl"), model_file(STATIC_PROP_FLAG | 0x4000, bench)).unwrap();
        std::fs::write(models.join("lamp.mdl"), model_file(STATIC_PROP_FLAG, "")).unwrap();
        std::fs::write(models.join("citizen.mdl"), model_file(0x4000, "mdlkeyvalue { }")).unwrap();
        std::fs::write(models.join("short.mdl"), &model_file(STATIC_PROP_FLAG, "")[..200]).unwrap();
        std::fs::write(models.join("other.mdl"), vec![b'x'; TRAITS_HEADER_BYTES]).unwrap();
        let filesystem = GameFs::from_gameinfo(&dir).unwrap();
        let read = |name: &str| prop_traits(&filesystem, &format!("Models\\Test\\{name}.mdl"));

        let bench = read("bench").unwrap();
        assert!(bench.static_prop);
        assert_eq!(bench.prop_data.unwrap().base.as_deref(), Some("Wooden.Large"));
        assert_eq!(
            read("lamp"),
            Some(PropTraits {
                static_prop: true,
                prop_data: None,
            })
        );
        assert_eq!(read("citizen"), Some(PropTraits::default()));
        assert_eq!(read("short"), None, "a file cut short is not read");
        assert_eq!(read("other"), None, "a file that is not a model is not read");
        assert_eq!(read("missing"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_quick_read_agrees_with_loading_the_whole_model() {
        let Some(dir) = std::env::var_os("MALLET_GAME_DIR") else {
            return;
        };
        let filesystem = GameFs::with_mounts(std::path::Path::new(&dir), &|_| false).unwrap();
        for model in [
            "models/props_c17/oildrum001.mdl",
            "models/props_c17/bench01a.mdl",
            "models/props_c17/lamppost03a_on.mdl",
            "models/props_c17/lamppost03a_off_dynamic.mdl",
            "models/humans/group01/male_07.mdl",
        ] {
            let (_, details) = load_model_variant(&filesystem, model, ModelVariant::default()).unwrap();
            assert_eq!(prop_traits(&filesystem, model), Some(details.prop_traits()), "{model}");
        }
        let bench = prop_traits(&filesystem, "models/props_c17/bench01a.mdl").unwrap();
        assert_eq!(bench.default_class(true), "prop_physics");
    }
}
